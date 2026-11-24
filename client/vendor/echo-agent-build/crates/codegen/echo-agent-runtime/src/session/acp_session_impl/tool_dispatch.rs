//! Tool dispatch helpers for `SessionActor`: `dispatch_tool` and its lock /
//! display helpers, direct bash-mode execution, and tool argument
//! parse-error formatting.

use super::*;

/// Number of output lines to show in final bash mode output summary
const BASH_MODE_FINAL_OUTPUT_LINES: usize = 10;
const BASH_MODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Dispatch once through the workspace toolset after admission and permission checks.
pub(super) async fn dispatch_tool(
    workspace_ops: &echo_agent_workspace::WorkspaceOps,
    prepared: &PreparedToolCall,
    session_id: &str,
) -> Result<ToolRunResult, echo_agent_tool_runtime::ToolError> {
    tracing::debug!(
        tool = %prepared.tool_name,
        call_id = %prepared.tool_call_id.0,
        session = %session_id,
        "dispatch_tool"
    );
    workspace_ops
        .call_tool(
            &prepared.tool_name,
            prepared.parsed_args.clone(),
            &prepared.tool_call_id.0,
            Some(session_id),
        )
        .await
}

/// First string-valued argument among `keys`, in priority order.
fn str_arg<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| args.get(*k)?.as_str())
}

/// Extract the workspace path that a tool call targets, for the purpose of
/// serializing concurrent same-file edits inside `execute_tool_calls`.
///
/// Different toolsets advertise the path under different JSON keys:
/// - `file_path` — echo_agent_build (`search_replace`), opencode (`EditTool`,
///   `WriteTool`, `ReadTool`), codex (`read_file`), echo_agent_build_hashline
///   (`hashline_edit`)
/// - `path` — alternate edit/read tools
/// - `target_file` — echo_agent_build (`read_file`, via `#[serde(rename)]`)
///
/// Returning the same string for two calls in a batch causes them to share a
/// `tokio::sync::Mutex` and therefore run sequentially in model-emitted order.
/// Returning `None` lets the call run fully concurrently with everything else.
///
/// `target_directory` is deliberately omitted — a directory listing isn't an
/// edit and must not bucket into a file lock.
pub(super) fn lock_path_for_args(args: &serde_json::Value) -> Option<&str> {
    str_arg(args, &["file_path", "path", "target_file"])
}

/// Pull the path a read/list tool targets and classify it against the store.
/// Keys span harnesses: `read_file`=`target_file`, grep=`path`,
/// `list_dir`=`target_directory`. Grammar lives in `echo_agent_compaction_transcript`.
pub(super) fn compaction_artifact_read(
    args: &serde_json::Value,
) -> Option<echo_agent_compaction_transcript::CompactionArtifact> {
    let path = str_arg(
        args,
        &["target_file", "file_path", "path", "target_directory"],
    )?;
    echo_agent_compaction_transcript::classify_compaction_path(path)
}

/// Map a backend-hosted tool name to a user-facing title, ACP ToolKind,
/// and `raw_input` JSON for display in the pager's tool call UI.
///
/// The `raw_input` carries metadata that the pager's `tool_call_to_block()`
/// uses to select the correct renderer (e.g., `variant: "WebSearch"` picks
/// the `WebSearchToolCallBlock` instead of the grep `SearchToolCallBlock`).
pub(super) fn backend_tool_display(name: &str) -> (String, acp::ToolKind, serde_json::Value) {
    match name {
        "web_search" => (
            "Web search:".to_string(),
            acp::ToolKind::Search,
            serde_json::json!({"variant": "WebSearch", "backend": true}),
        ),
        "x_search" => (
            "X search:".to_string(),
            acp::ToolKind::Search,
            serde_json::json!({"variant": "XSearch", "backend": true}),
        ),
        n => (
            n.to_string(),
            acp::ToolKind::Other,
            serde_json::json!({"backend": true}),
        ),
    }
}

/// Map a completed backend (server-side) tool call's payload to the ACP terminal
/// status the shell should emit. The backend reports each call's real
/// success/failure in the serialized payload's top-level `status` field (e.g. a
/// `web_search_call`'s `WebSearchToolCallStatus`, which includes `failed`); a
/// `"failed"` status becomes [`acp::ToolCallStatus::Failed`] so downstream
/// consumers — notably the headless `streaming-messages-json`
/// `web_search_tool_result_error` branch — see the real failure instead of a
/// blanket `Completed`. Any other or absent status stays `Completed`
/// (behavior-preserving for the success path).
pub(super) fn backend_tool_call_status(result: Option<&serde_json::Value>) -> acp::ToolCallStatus {
    let failed = result
        .and_then(|r| r.get("status"))
        .and_then(serde_json::Value::as_str)
        == Some("failed");
    if failed {
        acp::ToolCallStatus::Failed
    } else {
        acp::ToolCallStatus::Completed
    }
}

/// Temporary gate: only expose resolved model ID to the user for these models.
pub(super) fn should_show_resolved_model(requested: &str, resolved: &str) -> bool {
    requested != resolved && super::acp_types::is_coding_model_slug(requested)
}

/// Resolve the shell name for the system prompt `Shell:` field.
///
/// Unix: basename of `$SHELL` (e.g. "zsh", "bash").
/// Windows: name from the `detect_windows_shell` cascade
/// (pwsh > powershell.exe > Git Bash > cmd.exe), since `$SHELL` is absent.
pub(super) fn resolve_session_shell() -> String {
    #[cfg(unix)]
    {
        std::env::var("SHELL")
            .ok()
            .and_then(|s| {
                std::path::Path::new(&s)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "bash".to_string())
    }

    #[cfg(not(unix))]
    {
        echo_agent_config::shell::detect_windows_shell()
            .name()
            .to_string()
    }
}

/// Key in `ToolError::details` that carries the HTTP status code.
/// Used by both error producers (image_gen, video_gen, test helpers) and
/// the `is_auth_tool_error` classifier to avoid accidental key mismatch.
pub(crate) const HTTP_STATUS_DETAILS_KEY: &str = "status";

impl SessionActor {
    /// Extract bash command from prompt blocks if present in meta.
    /// Returns Some(command) if the prompt is a direct bash command, None otherwise.
    pub(super) fn extract_bash_command(prompt_blocks: &[acp::ContentBlock]) -> Option<String> {
        use crate::extensions::prompt_meta::PromptBlockMeta;
        for block in prompt_blocks {
            if let acp::ContentBlock::Text(text) = block
                && let Some(meta_val) = &text.meta
                && let Some(meta) = PromptBlockMeta::from_value(meta_val)
            {
                return meta.bash_command;
            }
        }
        None
    }

    /// Handle a direct bash command from bash mode.
    /// Runs the command with streaming output and sends updates to the TUI.
    pub(super) async fn handle_direct_bash_command(
        &self,
        _prompt_id: &str,
        command: String,
        prompt_blocks: &[acp::ContentBlock],
    ) -> PromptTurnResult {
        tracing::info!("Handling direct bash command");

        // Send user message chunks to scrollback (so user sees their command)
        let model_id = self.current_model_id().await;
        let user_chunk_meta = serde_json::json!({ "modelId": model_id })
            .as_object()
            .cloned();
        for block in prompt_blocks.iter() {
            let update = acp::SessionUpdate::UserMessageChunk(
                acp::ContentChunk::new(block.clone()).meta(user_chunk_meta.clone()),
            );
            let notification_meta = self.build_notification_meta();
            let _ = self
                .notifications
                .persistence_tx
                .send(PersistenceMsg::Update(SessionUpdate::Acp(Box::new(
                    acp::SessionNotification::new(self.session_info.id.clone(), update)
                        .meta(notification_meta.as_object().cloned()),
                ))));
        }

        // Persist the user message for session history
        let _ = self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::ContentChunk(PersistenceContentChunk::new(
                prompt_blocks.to_vec(),
            )));
        // Bash turns bypass `handle_prompt`'s commit point; the command is now
        // in the ordered persistence stream, so a send-now may cancel this turn.
        self.mark_front_message_committed().await;

        // Run the bash command with streaming enabled
        let tool_call_id = acp::ToolCallId::from(format!("bash-mode-{}", uuid::Uuid::new_v4()));

        // Send initial ToolCall to register with TUI

        use echo_agent_tools::types::ToolInput;
        // Use the stripped command as description so pager chrome shows the
        // real command (not a generic label) while still satisfying the required field.
        let title_command = echo_agent_tools::util::strip_redundant_session_cd(
            &command,
            self.tool_context.cwd.as_path(),
        );
        let tool_input = ToolInput::Bash(BashToolInput {
            command: command.clone(),
            timeout: None,
            description: title_command.clone().into_owned(),
            is_background: false,
        });
        // Bash mode has no model-issued wire name; resolve the toolset's
        // execute tool by kind so the echo.agent/tool identity still stamps.
        let bash_marker = serde_json::json!({"bash_mode": true}).as_object().cloned();
        let exec_wire = {
            let agent = self.agent.borrow();
            agent
                .tool_bridge()
                .toolset()
                .tool_name_for_kind(echo_agent_tools::types::tool::ToolKind::Execute)
        };
        let bash_meta = match exec_wire {
            Some(wire) => self.stamp_tool_meta(bash_marker.clone(), &wire, Some(&tool_input)),
            None => bash_marker,
        };
        self.send_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(tool_call_id.clone(), format!("Execute `{title_command}`"))
                    .kind(acp::ToolKind::Execute)
                    .status(acp::ToolCallStatus::InProgress)
                    .content(Vec::new())
                    .locations(Vec::new())
                    .raw_input(serde_json::to_value(&tool_input).ok())
                    .meta(bash_meta),
            ),
            None,
        )
        .await;

        let request = TerminalRunRequest {
            tool_call_id: tool_call_id.clone(),
            command: command.clone(),
            cwd: self.tool_context.cwd.clone(),
            env: self.tool_context.session_env.as_ref().clone(),
            timeout: BASH_MODE_TIMEOUT,
            output_byte_limit: 1_048_576, // 1 MiB
            stream: true,                 // Enable streaming for bash mode
            output_file: None,            // No file logging for interactive bash mode
        };

        let result = self.tool_context.terminal.run(request).await;

        // Format the output
        let (output, exit_code, timed_out, signal) = match result {
            Ok(res) => (
                res.combined_output,
                res.exit_code.unwrap_or(-1),
                res.timed_out,
                res.signal,
            ),
            Err(e) => (format!("Error running command: {}", e), -1, false, None),
        };

        // Create final summary with last N lines
        // Format: "... (X lines)\nlast\nfew\nlines"
        let lines: Vec<&str> = output.lines().collect();
        let total_lines = lines.len();
        let displayed_output = if total_lines > BASH_MODE_FINAL_OUTPUT_LINES {
            let start = total_lines - BASH_MODE_FINAL_OUTPUT_LINES;
            let last_lines = lines[start..].join("\n");
            format!("... ({} lines)\n{}", total_lines, last_lines)
        } else {
            output.trim_end().to_string()
        };

        let is_backgrounded = signal.as_deref() == Some("backgrounded");

        // Build the final response text with output summary and exit code
        let mut response_text = displayed_output.clone();
        if is_backgrounded {
            response_text.push_str("\n\n[command running in background]");
        } else if timed_out {
            response_text.push_str("\n\n[command timed out]");
        } else if let Some(ref sig) = signal {
            response_text.push_str(&format!("\n\n[killed by signal {}]", sig));
        } else {
            response_text.push_str(&format!("\n\n[exit code: {}]", exit_code));
        }

        // Send final tool call update
        // For backgrounded commands, don't mark as completed/failed - let the background task do that
        if !is_backgrounded {
            let final_status = if exit_code == 0 && signal.is_none() {
                acp::ToolCallStatus::Completed
            } else {
                acp::ToolCallStatus::Failed
            };
            let bash_output = BashOutput {
                output_for_prompt: BashOutput::make_output_for_prompt(&displayed_output),
                output: displayed_output.as_bytes().to_vec(),
                exit_code,
                command: command.clone(),
                truncated: total_lines > BASH_MODE_FINAL_OUTPUT_LINES,
                signal: signal.clone(),
                timed_out,
                description: None,
                current_dir: self.tool_context.cwd.to_string(),
                output_file: String::new(),
                total_bytes: displayed_output.len(),
                output_delta: None,
                was_bare_echo: false,
            };
            self.send_update(
                acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                    tool_call_id,
                    acp::ToolCallUpdateFields::new()
                        .status(Some(final_status))
                        .raw_output(serde_json::to_value(ToolsToolOutput::Bash(bash_output)).ok()),
                )),
                None,
            )
            .await;
        }

        // NOTE: The redundant AgentMessageChunk summary that was previously
        // sent here has been removed. The execute block already contains the
        // full command output — sending it again as an agent message created
        // a noisy duplicate scrollback entry. Old sessions that have it will
        // still replay fine; new sessions are cleaner.

        // Build a single user message for chat history that includes command, output, and exit code
        let user_message = format!(
            "I executed a terminal command: `{}`\n\nOutput:\n```\n{}\n```\n\n[exit code: {}]",
            command, displayed_output, exit_code
        );

        // Add to chat history as a user message only
        self.chat_state_handle
            .push_user_message(ConversationItem::user(&user_message));

        self.chat_state_handle.flush();

        let flush_error = self.flush_to_disk().await.err();
        self.disk_full_acp_error(flush_error.as_ref())?;

        let total_tokens = self.chat_state_handle.get_total_tokens().await;
        ok_end_turn(total_tokens, None)
    }
}

// ── Tool argument error formatting ─────────────────────────────────────

// Re-use the UTF-8-safe truncation helper from echo-agent-sampling-types rather
// than duplicating it here (R3).

/// Maximum bytes of `raw_arguments` included in a parse-error tool_result.
///
/// The model already holds the arguments in its recent context window, so
/// echoing the full string (potentially 8 KB+) would grow every subsequent
/// turn by that many tokens for no additional benefit.  The JSON error
/// position (e.g. `line 1 column 81`) is usually sufficient to locate the
/// typo; we include a prefix for orientation.
///
/// Note: when the JSON syntax error falls past this byte limit, the column
/// hint will reference text that was truncated from the message.  The model
/// should still have the full arguments in its context window from the
/// turn it generated them.
pub(crate) const MAX_ARGS_IN_ERROR: usize = 2_000;

/// Build the user-facing error message shown when tool arguments cannot be
/// parsed.  The message is stored as a `tool_result` in the conversation
/// history, so the model sees it on the very next turn.
///
/// The message intentionally includes:
///
/// 1. The normal error description (so the model knows *what* failed).
/// 2. The **original arguments string** the model produced (capped at
///    [`MAX_ARGS_IN_ERROR`] bytes).  Without this, echo-agent-shell would sanitize
///    the arguments to `"{}"` before forwarding them to the provider (to
///    avoid 400 errors), so the model would only see an empty object and have
///    to regenerate all its work from scratch.
/// 3. A JSON-level parse error (position + reason) when the arguments string
///    is itself invalid JSON — e.g. a missing `"` before a key name.  This
///    lets the model fix a one-character typo rather than regenerating a
///    thousand-line file.
pub(super) fn build_tool_parse_error_message(
    function_name: &str,
    err: &echo_agent_tool_runtime::ToolError,
    raw_arguments: &str,
) -> String {
    let mut msg = if function_name.trim().is_empty() {
        format!(
            "The model provider returned a tool call without a function name, so it could not be dispatched: {err}\n\
             Retry the call using exactly one of the advertised tool names."
        )
    } else {
        format!("Failed to parse arguments for tool `{function_name}`: {err}")
    };

    if raw_arguments.is_empty() {
        return msg;
    }

    // Append the original arguments (capped) so the model knows what it sent.
    // Use truncate_bytes to avoid panicking on a multi-byte UTF-8 boundary.
    msg.push_str("\n\nYour original arguments:\n");
    let prefix = truncate_bytes(raw_arguments, MAX_ARGS_IN_ERROR);
    msg.push_str(prefix);
    if prefix.len() < raw_arguments.len() {
        msg.push_str("\n... (truncated)");
    }

    // If the arguments string is not valid JSON, surface the exact position
    // of the syntax error so the model can fix it directly.
    // Use `IgnoredAny` — we only need the error, not a DOM.
    if let Err(json_err) = serde_json::from_str::<serde::de::IgnoredAny>(raw_arguments) {
        msg.push_str(&format!(
            "\n\nNote: the arguments above contain invalid JSON — {json_err}\n\
             Please fix the syntax and retry."
        ));
    }

    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_sampling_types::rs;

    fn web_search_payload(status: rs::WebSearchToolCallStatus) -> serde_json::Value {
        // The exact serialized `web_search_call` payload the sampler forwards on
        // `BackendToolCallCompleted` (via `serde_json::to_value(ws)`).
        serde_json::to_value(rs::WebSearchToolCall {
            action: rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                query: "rust async runtime".to_string(),
                sources: None,
            }),
            id: "ws1".to_string(),
            status,
        })
        .expect("serialize web_search_call payload")
    }

    /// A backend-reported web-search failure must map to ACP `Failed` (so the
    /// headless `web_search_tool_result_error` branch becomes reachable in
    /// production), while a completed call — or an absent payload — stays
    /// `Completed`. Exercises the real payload shape, not a hand-built status.
    #[test]
    fn backend_failed_web_search_maps_to_failed_status() {
        let failed = web_search_payload(rs::WebSearchToolCallStatus::Failed);
        assert_eq!(failed["status"], "failed", "wire field name is `status`");
        assert_eq!(
            backend_tool_call_status(Some(&failed)),
            acp::ToolCallStatus::Failed
        );

        let completed = web_search_payload(rs::WebSearchToolCallStatus::Completed);
        assert_eq!(
            backend_tool_call_status(Some(&completed)),
            acp::ToolCallStatus::Completed
        );

        // No payload at all is treated as success (behavior-preserving).
        assert_eq!(
            backend_tool_call_status(None),
            acp::ToolCallStatus::Completed
        );
    }
}
