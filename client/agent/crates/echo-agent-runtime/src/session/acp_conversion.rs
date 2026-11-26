//! ACP conversion functions for `echo-agent-tools`'s `ToolOutput`.
//!
//! These standalone functions convert `echo_agent_tools::types::output::ToolOutput`
//! into ACP protocol types (`acp::ToolCallUpdate`, `acp::Plan`).
//!
//! `raw_output` is serialized directly from ToolOutput via serde — no manual JSON
//! construction. The TUI deserializes it back into the same ToolOutput type, so
//! field names must match exactly. Path relativization for display happens on the
//! TUI side (which already has `base_path` for this purpose).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_client_protocol as acp;
use echo_agent_tool_types::{KillTaskOutput, TaskOutputOutput};
use echo_agent_tools::types::output::{
    ApplyPatchOutput, CodexGrepFilesOutput, ListDirOutput, MCPOutputDetails, ReadFileOutput,
    SearchReplaceEditContextInformation, SearchReplaceEditDetail, SearchReplaceOutput, ToolOutput,
};

/// Rewrites real worktree paths to display paths in serialized output.
///
/// In forked sessions, tools produce output containing the worktree
/// directory (e.g., `/root/.echo-agent/worktrees/project/fork-019cb252-...`). The
/// client UI should instead see the original project path (the `display_cwd`).
#[derive(Clone, Debug)]
pub(crate) struct PathRewriter {
    /// The real worktree path (what tools actually see).
    real_cwd: String,
    /// The display path (what the client UI should see).
    display_cwd: String,
}

impl PathRewriter {
    /// Create a new `PathRewriter` if `display_cwd` differs from `real_cwd`.
    ///
    /// Returns `None` if the paths are the same (no rewriting needed) or if
    /// `display_cwd` is not set.
    pub(crate) fn new(real_cwd: &str, display_cwd: Option<&str>) -> Option<Self> {
        let display_cwd = display_cwd?;
        if real_cwd == display_cwd {
            return None;
        }
        Some(Self {
            real_cwd: real_cwd.to_string(),
            display_cwd: display_cwd.to_string(),
        })
    }

    /// Rewrite all occurrences of the real worktree path with the display path.
    ///
    /// Handles both plain paths (e.g., `/root/.echo-agent/worktrees/project/fork-...`)
    /// and URL-encoded paths (e.g., `%2Froot%2F.echo_agent%2Fworktrees%2F...`) that
    /// appear in session directory structures and `output_file` references.
    pub(crate) fn rewrite(&self, text: &str) -> String {
        let plain = text.replace(&self.real_cwd, &self.display_cwd);
        // Also replace URL-encoded form — session directory paths use
        // urlencoding::encode(&cwd) as a path component, so background task
        // output_file paths and similar references contain encoded overlay cwd.
        let encoded_real = urlencoding::encode(&self.real_cwd);
        if plain.contains(encoded_real.as_ref()) {
            let encoded_display = urlencoding::encode(&self.display_cwd);
            plain.replace(encoded_real.as_ref(), encoded_display.as_ref())
        } else {
            plain
        }
    }

    /// Rewrite a `PathBuf` if it starts with the real worktree path.
    pub(crate) fn rewrite_path(&self, path: &Path) -> PathBuf {
        match path.strip_prefix(&self.real_cwd) {
            Ok(relative) => PathBuf::from(&self.display_cwd).join(relative),
            Err(_) => path.to_path_buf(),
        }
    }

    /// Rewrite a `serde_json::Value` by replacing paths in the serialized JSON string.
    ///
    /// Serialize to string, replace (plain + URL-encoded), re-parse. Catches
    /// paths embedded anywhere in the JSON tree without needing to walk the
    /// structure. Reuses `rewrite()` so both plain and encoded replacements
    /// are applied consistently.
    pub(crate) fn rewrite_json(&self, value: serde_json::Value) -> serde_json::Value {
        let serialized = value.to_string();
        let rewritten = self.rewrite(&serialized);
        if rewritten == serialized {
            return value;
        }
        serde_json::from_str(&rewritten).unwrap_or(value)
    }
}

/// Rewrite a string if a rewriter is present, otherwise return it unchanged.
pub(crate) fn maybe_rewrite(rewriter: Option<&PathRewriter>, text: String) -> String {
    match rewriter {
        Some(rw) => rw.rewrite(&text),
        None => text,
    }
}

/// Rewrite a path if a rewriter is present, otherwise return it unchanged.
fn maybe_rewrite_path(rewriter: Option<&PathRewriter>, path: PathBuf) -> PathBuf {
    match rewriter {
        Some(rw) => rw.rewrite_path(&path),
        None => path,
    }
}

/// Serialize ToolOutput to JSON for the `raw_output` field in ACP updates.
///
/// Uses serde directly — ToolOutput derives Serialize with `#[serde(tag = "type")]`,
/// so the JSON round-trips cleanly with the TUI's deserialization.
pub(crate) fn raw_output_json(
    output: &ToolOutput,
    rewriter: Option<&PathRewriter>,
) -> Option<serde_json::Value> {
    let value = serde_json::to_value(output).ok()?;
    Some(match rewriter {
        Some(rw) => rw.rewrite_json(value),
        None => value,
    })
}

/// Convert tool output to an ACP `ToolCallUpdate` for rich TUI rendering.
///
/// `Todo` output returns a minimal `Completed` update (the richer rendering
/// goes through `acp_plan_update` as a `Plan` notification).
///
/// `tool_meta` is attached as `_meta` on the update for MCP tools that have
/// MCP Apps UI metadata (e.g., `_meta.ui.resourceUri`). This allows clients
/// to render interactive UIs without maintaining a separate metadata store.
pub(crate) fn acp_tool_update(
    output: &ToolOutput,
    tool_call_id: &str,
    rewriter: Option<&PathRewriter>,
    tool_meta: Option<serde_json::Value>,
) -> Option<acp::ToolCallUpdate> {
    match output {
        ToolOutput::ReadFile(read_file_output) => {
            let (content, status) = match read_file_output {
                ReadFileOutput::FileContent(file_content) => {
                    let content = Some(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                        acp::TextContent::new(file_content.content.clone()),
                    ))]);
                    (content, acp::ToolCallStatus::Completed)
                }
                ReadFileOutput::FileNotFound(error_msg)
                | ReadFileOutput::IsADirectory(error_msg)
                | ReadFileOutput::PermissionDenied(error_msg)
                | ReadFileOutput::FileTooLarge(error_msg)
                | ReadFileOutput::FileReadError(error_msg) => {
                    let content = Some(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                        acp::TextContent::new(error_msg.clone()),
                    ))]);
                    (content, acp::ToolCallStatus::Failed)
                }
                ReadFileOutput::ImageContent(image_content) => {
                    // Construct the ACP `ImageContent` directly from the
                    // tool's local image type rather than going through a
                    // `From` impl on the tools crate -- that lets
                    // `echo-agent-tools` stay free of an
                    // `agent-client-protocol` dependency.
                    let content = Some(vec![acp::ToolCallContent::from(acp::ContentBlock::Image(
                        acp::ImageContent::new(
                            image_content.data.clone(),
                            image_content.mime_type.clone(),
                        )
                        .uri(image_content.uri.clone())
                        .meta(
                            image_content
                                .meta
                                .clone()
                                .and_then(|v| v.as_object().cloned()),
                        ),
                    ))]);
                    (content, acp::ToolCallStatus::Completed)
                }
                ReadFileOutput::ImageSizeError(error_msg) => {
                    let content = Some(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                        acp::TextContent::new(error_msg.clone()),
                    ))]);
                    (content, acp::ToolCallStatus::Failed)
                }
                ReadFileOutput::PdfPageImages(pdf) => {
                    let blocks: Vec<acp::ToolCallContent> = pdf
                        .pages
                        .iter()
                        .map(|page| {
                            acp::ToolCallContent::from(acp::ContentBlock::Image(
                                acp::ImageContent::new(page.data.clone(), page.mime_type.clone()),
                            ))
                        })
                        .collect();
                    (Some(blocks), acp::ToolCallStatus::Completed)
                }
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .content(content)
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::ListDir(list_dir_output) => {
            let status = if matches!(list_dir_output, ListDirOutput::Content(_)) {
                acp::ToolCallStatus::Completed
            } else {
                acp::ToolCallStatus::Failed
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::SearchReplace(search_replace_output) => {
            let (content, status) = match search_replace_output {
                SearchReplaceOutput::EditsApplied(edits_applied) => {
                    // Use absolute path for Diff content (TUI file opening).
                    let diff_path =
                        maybe_rewrite_path(rewriter, edits_applied.absolute_path.clone());
                    let content = Some(vec![acp::ToolCallContent::from(
                        acp::Diff::new(diff_path, edits_applied.new_string.clone())
                            .old_text(Some(edits_applied.old_string.clone()))
                            .meta(
                                serde_json::to_value(&edits_applied.edits)
                                    .ok()
                                    .and_then(|v| v.as_object().cloned()),
                            ),
                    )]);
                    (content, acp::ToolCallStatus::Completed)
                }
                SearchReplaceOutput::NoMatchesFound(e) => {
                    let content = Some(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                        acp::TextContent::new(e.message.clone()),
                    ))]);
                    (content, acp::ToolCallStatus::Failed)
                }
                SearchReplaceOutput::FileAlreadyExists(msg)
                | SearchReplaceOutput::MultipleMatchesFound(msg)
                | SearchReplaceOutput::InvalidInput(msg)
                | SearchReplaceOutput::FileNotFound(msg)
                | SearchReplaceOutput::FilenameTooLong(msg) => {
                    let content = Some(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                        acp::TextContent::new(msg.clone()),
                    ))]);
                    (content, acp::ToolCallStatus::Failed)
                }
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .content(content)
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::Bash(bash_output) => {
            let is_backgrounded = bash_output.signal.as_deref() == Some("backgrounded");
            let is_failure =
                bash_output.timed_out || (bash_output.signal.is_some() && !is_backgrounded);
            let status = if is_backgrounded {
                None
            } else if is_failure {
                Some(acp::ToolCallStatus::Failed)
            } else {
                Some(acp::ToolCallStatus::Completed)
            };
            let text = String::from_utf8_lossy(&bash_output.output).to_string();
            let text = maybe_rewrite(rewriter, text);
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(status)
                    .content(Some(vec![acp::ToolCallContent::from(
                        acp::ContentBlock::Text(acp::TextContent::new(text)),
                    )]))
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::GrepSearch(grep_search_output) => {
            let failed = grep_search_output.exit_code < 0 || grep_search_output.exit_code > 1;
            let status = if failed {
                acp::ToolCallStatus::Failed
            } else {
                acp::ToolCallStatus::Completed
            };
            let summary = if failed {
                output.to_prompt_format()
            } else {
                format!("found {} matches", grep_search_output.match_count)
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .content(Some(vec![acp::ToolCallContent::from(
                        acp::ContentBlock::Text(acp::TextContent::new(summary)),
                    )]))
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::WebSearch(_) => Some(acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from(tool_call_id)),
            acp::ToolCallUpdateFields::new()
                .status(Some(acp::ToolCallStatus::Completed))
                .raw_output(raw_output_json(output, rewriter)),
        )),
        // Web fetch output is converted to text content for the model.
        // Success (Content) → Completed; errors (DomainNotAllowed, CrossHostRedirect) → Failed.
        // This matches the pattern used by ReadFile, ListDir, and SearchReplace.
        ToolOutput::WebFetch(web_fetch_output) => {
            use echo_agent_tools::types::output::WebFetchOutput;
            let status = match web_fetch_output {
                WebFetchOutput::Content(_) => acp::ToolCallStatus::Completed,
                WebFetchOutput::DomainNotAllowed(_)
                | WebFetchOutput::CrossHostRedirect { .. }
                | WebFetchOutput::Error { .. } => acp::ToolCallStatus::Failed,
            };
            let text = web_fetch_output.to_prompt_format();
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .content(Some(vec![acp::ToolCallContent::from(
                        acp::ContentBlock::Text(acp::TextContent::new(text)),
                    )]))
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        // Todo also sends a Plan notification (see acp_plan_update), but we still
        // need to complete the tool call so the TUI flushes pending agent messages
        // and avoids concatenating text across tool-call boundaries.
        //
        // Error variants (e.g., DuplicateId) get `Failed` status so the Python
        // side can distinguish tool-logic errors from infra errors via raw_output.
        ToolOutput::Todo(todo_output) => {
            use echo_agent_tools::types::output::TodoWriteOutput;
            let (status, content) = match todo_output {
                TodoWriteOutput::TodosUpdated(_) => (acp::ToolCallStatus::Completed, None),
                TodoWriteOutput::DuplicateId(msg) | TodoWriteOutput::InvalidArgument(msg) => (
                    acp::ToolCallStatus::Failed,
                    Some(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                        acp::TextContent::new(msg.clone()),
                    ))]),
                ),
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .content(content)
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::MCP(mcp_output) => Some(
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(
                        if matches!(mcp_output.output(), MCPOutputDetails::Error(_)) {
                            acp::ToolCallStatus::Failed
                        } else {
                            acp::ToolCallStatus::Completed
                        },
                    ))
                    .raw_output(raw_output_json(output, rewriter)),
            )
            .meta(tool_meta.and_then(|v| serde_json::from_value(v).ok())),
        ),
        ToolOutput::BackgroundTaskStarted(bg) => {
            let short_id = if bg.task_id.len() > 8 {
                &bg.task_id[..8]
            } else {
                &bg.task_id
            };
            let title = maybe_rewrite(rewriter, format!("[bg] {} ({})", bg.command, short_id));
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(acp::ToolCallStatus::Completed))
                    .content(Some(vec![acp::ToolCallContent::from(
                        acp::ContentBlock::Text(acp::TextContent::new(bg.summary.clone())),
                    )]))
                    .raw_output(raw_output_json(output, rewriter))
                    .title(Some(title)),
            ))
        }
        ToolOutput::TaskOutput(task_output) => {
            let (status, title) = match task_output {
                TaskOutputOutput::Result(r) => {
                    let short_id = if r.task_id.len() > 8 {
                        &r.task_id[..8]
                    } else {
                        &r.task_id
                    };
                    (
                        acp::ToolCallStatus::Completed,
                        maybe_rewrite(rewriter, format!("{} ({})", r.command, short_id)),
                    )
                }
                TaskOutputOutput::TaskNotFound(_) => {
                    (acp::ToolCallStatus::Failed, "task not found".to_string())
                }
                TaskOutputOutput::MultiResult(mr) => (
                    acp::ToolCallStatus::Completed,
                    format!("multi-wait ({})", mr.mode),
                ),
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .raw_output(raw_output_json(output, rewriter))
                    .title(Some(title)),
            ))
        }
        ToolOutput::KillTask(kill_output) => {
            let (status, title) = match kill_output {
                KillTaskOutput::Result(r) => {
                    let short_id = if r.task_id.len() > 8 {
                        &r.task_id[..8]
                    } else {
                        &r.task_id
                    };
                    (
                        acp::ToolCallStatus::Completed,
                        format!("kill {} ({})", short_id, r.outcome),
                    )
                }
                KillTaskOutput::TaskNotFound(_) => {
                    (acp::ToolCallStatus::Failed, "task not found".to_string())
                }
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .raw_output(raw_output_json(output, rewriter))
                    .title(Some(title)),
            ))
        }
        ToolOutput::Skill(skill_output) => {
            let status = if skill_output.success {
                acp::ToolCallStatus::Completed
            } else {
                acp::ToolCallStatus::Failed
            };
            let title = if let Some(ref error) = skill_output.error {
                format!("Skill: {} - {}", skill_output.skill_name, error)
            } else {
                format!("Skill: {}", skill_output.skill_name)
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .content(Some(vec![acp::ToolCallContent::from(
                        acp::ContentBlock::Text(acp::TextContent::new(
                            skill_output.tool_result.clone(),
                        )),
                    )]))
                    .raw_output(raw_output_json(output, rewriter))
                    .title(Some(title)),
            ))
        }
        ToolOutput::ApplyPatch(apply_patch_output) => {
            let (content, status) = match apply_patch_output {
                ApplyPatchOutput::Success { files, .. } => {
                    // Send one acp::Diff per affected file — mirrors the
                    // SearchReplace pattern so the TUI can render inline diffs.
                    let content: Vec<acp::ToolCallContent> = files
                        .iter()
                        .map(|f| {
                            let old = f.old_text.as_deref().unwrap_or("");
                            let new = f.new_text.as_str();
                            let edits = build_apply_patch_edit_details(old, new);
                            let diff_path = maybe_rewrite_path(
                                rewriter,
                                f.move_to.clone().unwrap_or_else(|| f.path.clone()),
                            );
                            acp::ToolCallContent::from(
                                acp::Diff::new(diff_path, f.new_text.clone())
                                    .old_text(f.old_text.clone())
                                    .meta(
                                        serde_json::to_value(&edits)
                                            .ok()
                                            .and_then(|v| v.as_object().cloned()),
                                    ),
                            )
                        })
                        .collect();
                    (Some(content), acp::ToolCallStatus::Completed)
                }
                ApplyPatchOutput::ParseError(msg)
                | ApplyPatchOutput::ApplicationError(msg)
                | ApplyPatchOutput::EmptyPatch(msg) => {
                    let content = Some(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                        acp::TextContent::new(msg.clone()),
                    ))]);
                    (content, acp::ToolCallStatus::Failed)
                }
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .content(content)
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::CodexGrepFiles(grep_files_output) => {
            let status = match grep_files_output {
                CodexGrepFilesOutput::Matches { .. } | CodexGrepFilesOutput::NoMatches(_) => {
                    acp::ToolCallStatus::Completed
                }
                CodexGrepFilesOutput::Error(_) => acp::ToolCallStatus::Failed,
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(status))
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::SearchTool(out) => Some(acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from(tool_call_id)),
            acp::ToolCallUpdateFields::new()
                .status(Some(acp::ToolCallStatus::Completed))
                .content(Some(vec![acp::ToolCallContent::from(
                    acp::ContentBlock::Text(acp::TextContent::new(out.content.clone())),
                )]))
                .raw_output(raw_output_json(output, rewriter)),
        )),
        ToolOutput::Text(text) => Some(acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from(tool_call_id)),
            acp::ToolCallUpdateFields::new()
                .status(Some(acp::ToolCallStatus::Completed))
                .content(Some(vec![acp::ToolCallContent::from(
                    acp::ContentBlock::Text(acp::TextContent::new(text.text.clone())),
                )]))
                .raw_output(raw_output_json(output, rewriter)),
        )),
        // Dual channel: prose for non-pager clients, typed `raw_output` for the pager.
        ToolOutput::ImageGen(_)
        | ToolOutput::ImageToVideo(_)
        | ToolOutput::ReferenceToVideo(_)
        | ToolOutput::ImageEdit(_) => Some(acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from(tool_call_id)),
            acp::ToolCallUpdateFields::new()
                .status(Some(acp::ToolCallStatus::Completed))
                .content(Some(vec![acp::ToolCallContent::from(
                    acp::ContentBlock::Text(acp::TextContent::new(output.to_prompt_format())),
                )]))
                .raw_output(raw_output_json(output, rewriter)),
        )),
        ToolOutput::SubagentCompleted(sub) => {
            // Text includes resume handle for discoverability + meta for TUI.
            // Shared with the chat-bidi server via `to_model_text` so both
            // surfaces present a completed subagent identically.
            let content = Some(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                acp::TextContent::new(sub.to_model_text()),
            ))]);
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(acp::ToolCallStatus::Completed))
                    .content(content)
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::AskUserQuestion(ask) => {
            let message = match ask {
                echo_agent_tools::types::output::AskUserQuestionOutput::UserAnswered {
                    message,
                } => message.clone(),
                echo_agent_tools::types::output::AskUserQuestionOutput::QuestionsSent {
                    message,
                    ..
                } => message.clone(),
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(acp::ToolCallStatus::Completed))
                    .content(Some(vec![acp::ToolCallContent::from(
                        acp::ContentBlock::Text(acp::TextContent::new(message)),
                    )]))
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::EnterPlanMode(enter) => {
            let message = match enter {
                echo_agent_tools::types::output::EnterPlanModeOutput::Entered {
                    message, ..
                } => message.clone(),
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(acp::ToolCallStatus::Completed))
                    .title(Some("Plan mode entered".to_string()))
                    .content(Some(vec![acp::ToolCallContent::from(
                        acp::ContentBlock::Text(acp::TextContent::new(message)),
                    )]))
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::ExitPlanMode(exit) => {
            let message = match exit {
                echo_agent_tools::types::output::ExitPlanModeOutput::PlanReady {
                    message, ..
                } => message.clone(),
                echo_agent_tools::types::output::ExitPlanModeOutput::EmptyPlan {
                    message, ..
                } => message.clone(),
            };
            Some(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new()
                    .status(Some(acp::ToolCallStatus::Completed))
                    .title(Some("Plan mode exited".to_string()))
                    .content(Some(vec![acp::ToolCallContent::from(
                        acp::ContentBlock::Text(acp::TextContent::new(message)),
                    )]))
                    .raw_output(raw_output_json(output, rewriter)),
            ))
        }
        ToolOutput::UpdateGoal(_)
        | ToolOutput::Workflow(_)
        | ToolOutput::Monitor(_)
        | ToolOutput::SchedulerCreate(_)
        | ToolOutput::SchedulerDelete(_)
        | ToolOutput::SchedulerList(_) => Some(acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from(tool_call_id)),
            acp::ToolCallUpdateFields::new()
                .status(Some(acp::ToolCallStatus::Completed))
                .raw_output(raw_output_json(output, rewriter)),
        )),
        // Internal tools (open_page, browse_page, etc.) are not used in the
        // shell — they are server-only.  This arm covers variants that appear
        // when Cargo unifies the optional web-tools feature across the workspace.
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

/// Convert a `Todo` tool output to an ACP `Plan` notification.
///
/// Returns `None` for non-Todo outputs.
///
/// This converts `echo-agent-tools`' TodoItem (which has `id`, `content: Option<String>`,
/// `status: Option<String>`) to `acp::PlanEntry` (which has `content`, `priority`, `status`).
/// The `id` is not directly represented in `PlanEntry` but the ordering is preserved.
pub(crate) fn acp_plan_update(output: &ToolOutput) -> Option<acp::Plan> {
    use crate::tools::todo::plan_entry_from_todo_item;
    use echo_agent_tools::types::output::TodoWriteOutput;
    match output {
        ToolOutput::Todo(TodoWriteOutput::TodosUpdated(success)) => {
            let entries = success
                .todos
                .iter()
                .cloned()
                .map(plan_entry_from_todo_item)
                .collect();
            Some(acp::Plan::new(entries))
        }
        // Error variants (DuplicateId, etc.) don't produce Plan updates.
        _ => None,
    }
}

/// Build `SearchReplaceEditContextInformation` from full old/new file content
/// for an apply_patch file result. Extracts contiguous changed regions so the
/// renderer gets accurate line numbers, old/new strings, and surrounding context.
fn build_apply_patch_edit_details(
    old_content: &str,
    new_content: &str,
) -> SearchReplaceEditContextInformation {
    const CONTEXT_LINES: usize = 3;

    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    // Walk both line arrays to find contiguous changed regions.
    let mut details = Vec::new();
    let max_len = old_lines.len().max(new_lines.len());
    let mut i = 0;
    while i < max_len {
        let old_line = old_lines.get(i).copied();
        let new_line = new_lines.get(i).copied();

        if old_line != new_line {
            // Found start of a changed region. Scan forward to find the end.
            let region_start = i;
            let mut old_end = i;
            let mut new_end = i;

            while old_end < old_lines.len() || new_end < new_lines.len() {
                let ol = old_lines.get(old_end).copied();
                let nl = new_lines.get(new_end).copied();
                if ol == nl {
                    break;
                }
                if old_end < old_lines.len() {
                    old_end += 1;
                }
                if new_end < new_lines.len() {
                    new_end += 1;
                }
            }

            let old_string = old_lines[region_start..old_end].join("\n");
            let new_string = new_lines[region_start..new_end].join("\n");

            // Context before: up to CONTEXT_LINES lines before the change.
            let ctx_before_start = region_start.saturating_sub(CONTEXT_LINES);
            let context_before = old_lines[ctx_before_start..region_start].join("\n");

            // Context after: up to CONTEXT_LINES lines after the change.
            let ctx_after_end = (old_end + CONTEXT_LINES).min(old_lines.len());
            let context_after = old_lines[old_end..ctx_after_end].join("\n");

            details.push(SearchReplaceEditDetail {
                old_string,
                old_line: region_start + 1, // 1-based
                new_string,
                new_line: region_start + 1, // 1-based
                context_before,
                context_after,
                line_prefix: String::new(),
            });

            i = old_end.max(new_end);
        } else {
            i += 1;
        }
    }

    // If no differences found (e.g., add or delete), create a single entry
    // covering the whole content.
    if details.is_empty() {
        details.push(SearchReplaceEditDetail {
            old_string: old_content.to_string(),
            old_line: 1,
            new_string: new_content.to_string(),
            new_line: 1,
            context_before: String::new(),
            context_after: String::new(),
            line_prefix: String::new(),
        });
    }

    SearchReplaceEditContextInformation { details }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_tools::types::output::*;
    use std::path::PathBuf;

    #[test]
    fn test_acp_tool_update_read_file_success() {
        let output = ToolOutput::ReadFile(ReadFileOutput::FileContent(FileContent {
            document_progress: None,
            content: "file content".to_string(),
            content_concise: None,
            absolute_path: PathBuf::from("/project/src/main.rs"),
            offset: None,
            limit: None,
            raw_output: "file content".to_string(),
            total_lines: 100,
            extracted_images: Vec::new(),
        }));
        let update = acp_tool_update(&output, "call-1", None, None).unwrap();
        assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));
        assert!(update.fields.content.is_some());
    }

    #[test]
    fn test_acp_tool_update_todo_returns_completed() {
        let output = ToolOutput::Todo(TodoWriteOutput::TodosUpdated(TodoWriteSuccess {
            summary_for_prompt: "tasks".to_string(),
            todos: vec![],
            state: echo_agent_tools::implementations::echo_agent_build::todo::TodoState::default(),
        }));
        let update = acp_tool_update(&output, "call-1", None, None).unwrap();
        assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));
    }

    #[test]
    fn test_turn_end_plan_cleanup_preserves_semantics_and_priority() {
        use crate::tools::todo::plan_entry_from_todo_item;
        use echo_agent_tools::implementations::echo_agent_build::todo::{
            TodoItem, TodoPriority, TodoStatus,
        };

        // Simulate a mixed todo list at turn end.
        let items = [
            TodoItem {
                content: "Done".to_string(),
                priority: TodoPriority::Medium,
                status: TodoStatus::Completed,
                meta: None,
            },
            TodoItem {
                content: "Dropped".to_string(),
                priority: TodoPriority::Low,
                status: TodoStatus::Cancelled,
                meta: None,
            },
            TodoItem {
                content: "Stale spinner".to_string(),
                priority: TodoPriority::High,
                status: TodoStatus::InProgress,
                meta: None,
            },
        ];

        // Build plan entries using the canonical helper, then override
        // in_progress → completed (same logic as emit_turn_end_plan_cleanup).
        let entries: Vec<acp::PlanEntry> = items
            .iter()
            .map(|item| {
                let mut entry = plan_entry_from_todo_item(item.clone());
                if item.status == TodoStatus::InProgress {
                    entry.status = acp::PlanEntryStatus::Completed;
                }
                entry
            })
            .collect();

        // Completed item: unchanged, medium priority preserved
        assert_eq!(entries[0].status, acp::PlanEntryStatus::Completed);
        assert_eq!(entries[0].priority, acp::PlanEntryPriority::Medium);
        assert!(entries[0].meta.is_none());

        // Cancelled item: Completed with cancelled marker, LOW priority preserved
        assert_eq!(entries[1].status, acp::PlanEntryStatus::Completed);
        assert_eq!(entries[1].priority, acp::PlanEntryPriority::Low);
        assert_eq!(entries[1].meta.as_ref().unwrap()["cancelled"], true);

        // In-progress item: overridden to Completed, HIGH priority preserved
        assert_eq!(entries[2].status, acp::PlanEntryStatus::Completed);
        assert_eq!(entries[2].priority, acp::PlanEntryPriority::High);
        // No cancelled marker (it was in_progress, not cancelled)
        assert!(entries[2].meta.is_none());
    }

    #[test]
    fn test_acp_tool_update_todo_duplicate_id_returns_failed() {
        let output = ToolOutput::Todo(TodoWriteOutput::DuplicateId(
            "Duplicate todo ID: \"dup\"".to_string(),
        ));
        let update = acp_tool_update(&output, "call-1", None, None).unwrap();
        assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Failed));
        assert!(update.fields.content.is_some());
    }

    #[test]
    fn test_acp_plan_update_todo() {
        let output = ToolOutput::Todo(TodoWriteOutput::TodosUpdated(TodoWriteSuccess {
            summary_for_prompt: "tasks".to_string(),
            todos: vec![
                echo_agent_tools::implementations::echo_agent_build::todo::TodoItem {
                    content: "Task 1".to_string(),
                    priority:
                        echo_agent_tools::implementations::echo_agent_build::todo::TodoPriority::Medium,
                    status:
                        echo_agent_tools::implementations::echo_agent_build::todo::TodoStatus::Completed,
                    meta: None,
                },
            ],
            state: echo_agent_tools::implementations::echo_agent_build::todo::TodoState::default(),
        }));
        let plan = acp_plan_update(&output).unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].content, "Task 1");
        assert_eq!(plan.entries[0].status, acp::PlanEntryStatus::Completed);
    }

    #[test]
    fn test_acp_plan_update_todo_duplicate_id_returns_none() {
        let output = ToolOutput::Todo(TodoWriteOutput::DuplicateId(
            "Duplicate todo ID: \"dup\"".to_string(),
        ));
        assert!(acp_plan_update(&output).is_none());
    }

    #[test]
    fn test_acp_plan_update_non_todo_returns_none() {
        let output = ToolOutput::GrepSearch(GrepSearchOutput {
            stdout: vec![],
            stderr: vec![],
            exit_code: 0,
            match_count: 0,
            file_matches: vec![],
        });
        assert!(acp_plan_update(&output).is_none());
    }

    #[test]
    fn test_acp_grep_failure_is_not_reported_as_completed() {
        let output = ToolOutput::GrepSearch(GrepSearchOutput {
            stdout: Vec::new(),
            stderr: b"failed to launch rg".to_vec(),
            exit_code: -1,
            match_count: 0,
            file_matches: vec![],
        });
        let update = acp_tool_update(&output, "call-grep", None, None).unwrap();
        assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Failed));
        let content = update.fields.content.expect("failure content");
        match &content[0] {
            acp::ToolCallContent::Content(acp::Content {
                content: acp::ContentBlock::Text(text),
                ..
            }) => assert!(text.text.contains("failed to launch rg")),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn test_acp_grep_no_match_is_completed() {
        let output = ToolOutput::GrepSearch(GrepSearchOutput {
            stdout: b"No matches found".to_vec(),
            stderr: Vec::new(),
            exit_code: 1,
            match_count: 0,
            file_matches: vec![],
        });
        let update = acp_tool_update(&output, "call-grep", None, None).unwrap();
        assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));
    }

    #[test]
    fn test_raw_output_json_round_trips() {
        // raw_output_json uses serde directly, so it must round-trip with ToolOutput deserialization
        let output = ToolOutput::ReadFile(ReadFileOutput::FileContent(FileContent {
            document_progress: None,
            content: "content".to_string(),
            content_concise: None,
            absolute_path: PathBuf::from("/project/src/main.rs"),
            offset: None,
            limit: None,
            raw_output: "content".to_string(),
            total_lines: 100,
            extracted_images: Vec::new(),
        }));
        let json = raw_output_json(&output, None).unwrap();
        // Verify it deserializes back into the same type
        let round_tripped: ToolOutput = serde_json::from_value(json).unwrap();
        match round_tripped {
            ToolOutput::ReadFile(ReadFileOutput::FileContent(fc)) => {
                assert_eq!(fc.absolute_path, PathBuf::from("/project/src/main.rs"));
                assert_eq!(fc.content, "content");
            }
            other => panic!("Expected ReadFile, got {:?}", other),
        }
    }

    #[test]
    fn test_acp_tool_update_text_completes_with_content() {
        let output = ToolOutput::Text(
            "Found 3 memory result(s):\n\n### Result 1"
                .to_string()
                .into(),
        );
        let update = acp_tool_update(&output, "call-mem", None, None).unwrap();
        assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));

        let content = update.fields.content.as_ref().expect("should have content");
        assert_eq!(content.len(), 1);
        match &content[0] {
            acp::ToolCallContent::Content(acp::Content {
                content: acp::ContentBlock::Text(tc),
                ..
            }) => assert!(tc.text.contains("Found 3 memory result(s)")),
            other => panic!("expected Text content, got {other:?}"),
        }

        // ToolOutput::Text wraps TextOutput { text: String }, which serde can
        // serialize with internal tagging. raw_output carries the JSON.
        assert!(update.fields.raw_output.is_some());
    }

    #[test]
    fn test_raw_output_json_list_dir_round_trips() {
        let output = ToolOutput::ListDir(ListDirOutput::Content(ListDirContent {
            content: "file1.rs\nfile2.rs".to_string(),
            absolute_root_path: PathBuf::from("/project/src"),
        }));
        let json = raw_output_json(&output, None).unwrap();
        let round_tripped: ToolOutput = serde_json::from_value(json).unwrap();
        match round_tripped {
            ToolOutput::ListDir(ListDirOutput::Content(ldc)) => {
                assert_eq!(ldc.absolute_root_path, PathBuf::from("/project/src"));
            }
            other => panic!("Expected ListDir, got {:?}", other),
        }
    }
    // --- PathRewriter tests ---

    #[test]
    fn test_path_rewriter_new_returns_none_when_same() {
        assert!(PathRewriter::new("/project", Some("/project")).is_none());
    }

    #[test]
    fn test_path_rewriter_new_returns_none_when_no_display() {
        assert!(PathRewriter::new("/project", None).is_none());
    }

    #[test]
    fn test_path_rewriter_new_returns_some_when_different() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/project/ab-123",
            Some("/home/user/project"),
        );
        assert!(rw.is_some());
    }

    #[test]
    fn test_path_rewriter_rewrite_text() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/myproject/ab-123",
            Some("/testbed/myproject"),
        )
        .unwrap();
        let input = "File at /root/.echo-agent/worktrees/myproject/ab-123/src/main.rs";
        let output = rw.rewrite(input);
        assert_eq!(output, "File at /testbed/myproject/src/main.rs");
    }

    #[test]
    fn test_path_rewriter_rewrite_path() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/myproject/ab-123",
            Some("/testbed/myproject"),
        )
        .unwrap();
        let path = Path::new("/root/.echo-agent/worktrees/myproject/ab-123/src/lib.rs");
        let rewritten = rw.rewrite_path(path);
        assert_eq!(rewritten, PathBuf::from("/testbed/myproject/src/lib.rs"));
    }

    #[test]
    fn test_path_rewriter_rewrite_path_no_match() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/myproject/ab-123",
            Some("/testbed/myproject"),
        )
        .unwrap();
        let path = Path::new("/other/path/file.rs");
        let rewritten = rw.rewrite_path(path);
        assert_eq!(rewritten, PathBuf::from("/other/path/file.rs"));
    }

    #[test]
    fn test_path_rewriter_rewrites_raw_output_json() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/myproject/ab-123",
            Some("/testbed/myproject"),
        )
        .unwrap();
        let output = ToolOutput::ReadFile(ReadFileOutput::FileContent(FileContent {
            document_progress: None,
            content: "content".to_string(),
            content_concise: None,
            absolute_path: PathBuf::from(
                "/root/.echo-agent/worktrees/myproject/ab-123/src/main.rs",
            ),
            offset: None,
            limit: None,
            raw_output: "content".to_string(),
            total_lines: 100,
            extracted_images: Vec::new(),
        }));
        let json = raw_output_json(&output, Some(&rw)).unwrap();
        let round_tripped: ToolOutput = serde_json::from_value(json).unwrap();
        match round_tripped {
            ToolOutput::ReadFile(ReadFileOutput::FileContent(fc)) => {
                assert_eq!(
                    fc.absolute_path,
                    PathBuf::from("/testbed/myproject/src/main.rs")
                );
            }
            other => panic!("Expected ReadFile, got {:?}", other),
        }
    }

    #[test]
    fn test_media_gen_acp_update_emits_prose_and_raw_output() {
        // Dual channel: prompt-format JSON in content, typed variant in raw_output.
        let output = ToolOutput::ImageToVideo(MediaGenOutput::new(PathBuf::from(
            "/tmp/session/videos/3.mp4",
        )));
        let update = acp_tool_update(&output, "tc-1", None, None).expect("update");
        let content = update.fields.content.expect("content");
        let text = match &content[0] {
            acp::ToolCallContent::Content(acp::Content {
                content: acp::ContentBlock::Text(t),
                ..
            }) => t.text.clone(),
            other => panic!("expected text content, got {other:?}"),
        };
        let prompt_json: serde_json::Value = serde_json::from_str(&text).expect("prompt json");
        assert_eq!(prompt_json["path"], "/tmp/session/videos/3.mp4");
        assert_eq!(prompt_json["filename"], "3.mp4");
        assert_eq!(prompt_json["session_folder"], "videos");
        assert_eq!(
            prompt_json["message"],
            "Video generated and saved to /tmp/session/videos/3.mp4. Do not read or re-display it, and do not describe how it appears to the user."
        );
        let raw = update.fields.raw_output.expect("raw_output");
        assert_eq!(raw["type"], "ImageToVideo");
        assert_eq!(raw["path"], "/tmp/session/videos/3.mp4");
    }

    #[test]
    fn test_path_rewriter_rewrites_list_dir_raw_output() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/myproject/ab-123",
            Some("/testbed/myproject"),
        )
        .unwrap();
        let output = ToolOutput::ListDir(ListDirOutput::Content(ListDirContent {
            content: "file1.rs\nfile2.rs".to_string(),
            absolute_root_path: PathBuf::from("/root/.echo-agent/worktrees/myproject/ab-123/src"),
        }));
        let json = raw_output_json(&output, Some(&rw)).unwrap();
        let round_tripped: ToolOutput = serde_json::from_value(json).unwrap();
        match round_tripped {
            ToolOutput::ListDir(ListDirOutput::Content(ldc)) => {
                assert_eq!(
                    ldc.absolute_root_path,
                    PathBuf::from("/testbed/myproject/src")
                );
            }
            other => panic!("Expected ListDir, got {:?}", other),
        }
    }

    #[test]
    fn test_path_rewriter_rewrites_bash_command_and_output() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/myproject/ab-123",
            Some("/testbed/myproject"),
        )
        .unwrap();
        let output = ToolOutput::Bash(BashOutput {
            output: b"listing /root/.echo-agent/worktrees/myproject/ab-123/src".to_vec(),
            output_for_prompt: String::new(),
            exit_code: 0,
            command: "ls /root/.echo-agent/worktrees/myproject/ab-123/src".to_string(),
            truncated: false,
            signal: None,
            timed_out: false,
            description: None,
            current_dir: "/root/.echo-agent/worktrees/myproject/ab-123".to_string(),
            output_file: "/tmp/output.txt".to_string(),
            output_delta: None,
            total_bytes: 0,
            was_bare_echo: false,
        });
        let json = raw_output_json(&output, Some(&rw)).unwrap();
        let round_tripped: ToolOutput = serde_json::from_value(json).unwrap();
        match round_tripped {
            ToolOutput::Bash(bash) => {
                assert_eq!(bash.command, "ls /testbed/myproject/src");
                assert_eq!(bash.current_dir, "/testbed/myproject");
            }
            other => panic!("Expected Bash, got {:?}", other),
        }
    }

    #[test]
    fn test_path_rewriter_rewrites_search_replace_diff_path() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/myproject/ab-123",
            Some("/testbed/myproject"),
        )
        .unwrap();
        let output = ToolOutput::SearchReplace(SearchReplaceOutput::EditsApplied(
            SearchReplaceEditsApplied {
                old_string: "old".to_string(),
                new_string: "new".to_string(),
                tool_output_for_prompt: String::new(),
                tool_output_for_prompt_concise: None,
                absolute_path: PathBuf::from(
                    "/root/.echo-agent/worktrees/myproject/ab-123/src/lib.rs",
                ),
                edits: SearchReplaceEditContextInformation::default(),
                patch: None,
                unicode_normalized: false,
            },
        ));
        let update = acp_tool_update(&output, "call-1", Some(&rw), None).unwrap();
        let content = update.fields.content.unwrap();
        assert_eq!(content.len(), 1);
        match &content[0] {
            acp::ToolCallContent::Diff(diff) => {
                assert_eq!(diff.path, PathBuf::from("/testbed/myproject/src/lib.rs"));
            }
            other => panic!("Expected Diff, got {:?}", other),
        }
        let raw = update.fields.raw_output.unwrap();
        let raw_str = raw.to_string();
        assert!(
            !raw_str.contains("/root/.echo-agent/worktrees/myproject/ab-123"),
            "raw_output should not contain worktree path, got: {}",
            raw_str
        );
        assert!(raw_str.contains("/testbed/myproject/src/lib.rs"));
    }

    // ── URL-encoded path rewriting ─────────────────────────────────

    #[test]
    fn test_rewrite_handles_url_encoded_paths() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/project/ab-123-a-overlay",
            Some("/home/user/project"),
        )
        .unwrap();
        // Session directory paths use urlencoding::encode(&cwd)
        let encoded_overlay =
            urlencoding::encode("/root/.echo-agent/worktrees/project/ab-123-a-overlay");
        let input = format!(
            "output-file: /root/.echo-agent/sessions/{}/session-id/terminal/call.log",
            encoded_overlay
        );
        let result = rw.rewrite(&input);
        assert!(
            !result.contains("ab-123-a-overlay"),
            "URL-encoded overlay path must be rewritten: {result}"
        );
        let encoded_display = urlencoding::encode("/home/user/project");
        assert!(
            result.contains(encoded_display.as_ref()),
            "rewritten output must contain the encoded display path: {result}"
        );
    }

    #[test]
    fn test_rewrite_handles_plain_paths() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/project/ab-123-a-overlay",
            Some("/home/user/project"),
        )
        .unwrap();
        let input = "file: /root/.echo-agent/worktrees/project/ab-123-a-overlay/src/main.rs";
        let result = rw.rewrite(input);
        assert_eq!(result, "file: /home/user/project/src/main.rs");
    }

    #[test]
    fn test_rewrite_json_handles_url_encoded_paths() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/project/ab-123",
            Some("/testbed/project"),
        )
        .unwrap();
        let encoded = urlencoding::encode("/root/.echo-agent/worktrees/project/ab-123");
        let value = serde_json::json!({
            "output_file": format!("/sessions/{}/task.log", encoded),
            "status": "running",
        });
        let rewritten = rw.rewrite_json(value);
        let output_file = rewritten["output_file"].as_str().unwrap();
        assert!(
            !output_file.contains("ab-123"),
            "rewrite_json must handle URL-encoded paths: {output_file}"
        );
    }

    #[test]
    fn test_rewrite_noop_when_no_overlay_path_present() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/project/ab-123",
            Some("/testbed/project"),
        )
        .unwrap();
        let input = "exit: 0\nhello world\n";
        assert_eq!(
            rw.rewrite(input),
            input,
            "text without overlay paths must pass through unchanged"
        );
    }

    #[test]
    fn test_maybe_rewrite_with_none_returns_original() {
        let original = "Tool `read_file` failed: /some/path".to_string();
        let result = maybe_rewrite(None, original.clone());
        assert_eq!(result, original);
    }

    #[test]
    fn test_maybe_rewrite_with_rewriter_sanitizes_error_text() {
        let rw = PathRewriter::new(
            "/root/.echo-agent/worktrees/project/ab-999",
            Some("/home/user/project"),
        )
        .unwrap();
        let error_text = "Tool `read_file` failed: IO error reading /root/.echo-agent/worktrees/project/ab-999/src/lib.rs".to_string();
        let result = maybe_rewrite(Some(&rw), error_text);
        assert!(
            !result.contains("ab-999"),
            "error text must not contain the overlay path: {result}"
        );
        assert!(
            result.contains("/home/user/project/src/lib.rs"),
            "error text must show the display path: {result}"
        );
    }

    #[test]
    fn test_no_rewriter_preserves_original_paths() {
        let output = ToolOutput::ReadFile(ReadFileOutput::FileContent(FileContent {
            document_progress: None,
            content: "content".to_string(),
            content_concise: None,
            absolute_path: PathBuf::from(
                "/root/.echo-agent/worktrees/myproject/ab-123/src/main.rs",
            ),
            offset: None,
            limit: None,
            raw_output: "content".to_string(),
            total_lines: 100,
            extracted_images: Vec::new(),
        }));
        let json = raw_output_json(&output, None).unwrap();
        let round_tripped: ToolOutput = serde_json::from_value(json).unwrap();
        match round_tripped {
            ToolOutput::ReadFile(ReadFileOutput::FileContent(fc)) => {
                assert_eq!(
                    fc.absolute_path,
                    PathBuf::from("/root/.echo-agent/worktrees/myproject/ab-123/src/main.rs")
                );
            }
            other => panic!("Expected ReadFile, got {:?}", other),
        }
    }

    #[test]
    fn test_acp_tool_update_pdf_page_images() {
        let output = ToolOutput::ReadFile(ReadFileOutput::PdfPageImages(PdfPageImages {
            document_progress: None,
            pages: vec![
                PdfPageImage {
                    data: "base64_page1".to_string(),
                    mime_type: "image/jpeg".to_string(),
                    page_number: 1,
                },
                PdfPageImage {
                    data: "base64_page2".to_string(),
                    mime_type: "image/jpeg".to_string(),
                    page_number: 2,
                },
            ],
            total_pages: 10,
            file_size: 4096,
        }));
        let update = acp_tool_update(&output, "call-pdf", None, None).unwrap();
        assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));
        let content = update.fields.content.as_ref().expect("should have content");
        assert_eq!(content.len(), 2, "should have 2 image blocks");
        for block in content {
            match block {
                acp::ToolCallContent::Content(acp::Content {
                    content: acp::ContentBlock::Image(img),
                    ..
                }) => {
                    assert_eq!(img.mime_type, "image/jpeg");
                }
                other => panic!("expected Image block, got {other:?}"),
            }
        }
    }
}
