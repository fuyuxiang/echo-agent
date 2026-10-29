//! Tool dispatch helpers for `SessionActor`: `dispatch_tool` and its lock /
//! display helpers, direct bash-mode execution, and tool argument
//! parse-error formatting.

use super::*;

/// Number of output lines to show in final bash mode output summary
const BASH_MODE_FINAL_OUTPUT_LINES: usize = 10;
const BASH_MODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Per-turn opt-in set from trusted ACP block metadata by the desktop host.
#[derive(Clone, Copy, Default)]
pub(super) struct DesktopDocumentTools(pub bool);

#[derive(Clone)]
pub(super) struct DesktopDocumentBridge {
    pub gateway: GatewaySender,
    pub authority: Option<crate::session::InheritedTurnAuthority>,
    pub run_id: String,
    pub workspace: String,
    pub tool_kinds: std::collections::HashMap<String, String>,
}

/// Phase 2: dispatch a tool call through [`WorkspaceOps::call_tool`].
///
/// Agent sessions always use local workspace ops (in-process toolset).
pub(super) async fn dispatch_tool(
    workspace_ops: &echo_agent_workspace::WorkspaceOps,
    prepared: &PreparedToolCall,
    session_id: &str,
    desktop: Option<&DesktopDocumentBridge>,
) -> Result<ToolRunResult, echo_agent_tool_runtime::ToolError> {
    let external = desktop.is_some_and(|bridge| {
        bridge
            .tool_kinds
            .get(&prepared.tool_name)
            .is_some_and(|kind| kind == "use_tool")
            || prepared.tool_name.contains("__")
    });
    if external {
        let ops = workspace_ops.clone();
        let prepared = prepared.clone();
        let session = session_id.to_string();
        let desktop = desktop.cloned();
        // Dropping a Tokio JoinHandle detaches it. A user Stop aborts the
        // model turn, but must keep this actual RPC alive until a protocol
        // terminal (or an explicit uncertain result) settles its native lease.
        return tokio::spawn(async move {
            dispatch_tool_inner(&ops, &prepared, &session, desktop.as_ref(), true).await
        }).await.map_err(|_| echo_agent_tool_runtime::ToolError::invalid_arguments(
            "MCP execution task ended without a terminal; native workspace fence remains pending"
        ))?;
    }
    dispatch_tool_inner(workspace_ops, prepared, session_id, desktop, false).await
}

async fn dispatch_tool_inner(
    workspace_ops: &echo_agent_workspace::WorkspaceOps,
    prepared: &PreparedToolCall,
    session_id: &str,
    desktop: Option<&DesktopDocumentBridge>,
    external: bool,
) -> Result<ToolRunResult, echo_agent_tool_runtime::ToolError> {
    // Task itself only schedules a child; its individual tools are intercepted
    // by the child's SessionActor using the inherited root authority.
    let desktop = desktop.filter(|bridge| {
        bridge
            .tool_kinds
            .get(&prepared.tool_name)
            .is_none_or(|kind| kind != "task")
    });
    let authority = desktop.and_then(|bridge| bridge.authority.as_ref());
    let owner_session_id = authority
        .map(|authority| authority.session_id.as_str())
        .unwrap_or(session_id);
    let run_id = authority
        .map(|authority| authority.run_id.as_str())
        .or_else(|| desktop.map(|bridge| bridge.run_id.as_str()));
    // Model tool IDs can repeat in later turns or across children. Namespace
    // the opaque editor identity so an old timeout cannot remove a new entry.
    let operation_id = format!(
        "{}:{session_id}:{}",
        run_id.unwrap_or_default(),
        prepared.tool_call_id.0
    );
    tracing::debug!(
        tool = %prepared.tool_name,
        call_id = %prepared.tool_call_id.0,
        session = %session_id,
        mode = "local",
        "dispatch_tool"
    );
    if let Some(desktop) = desktop {
        let params = serde_json::json!({
            "phase": "before", "sessionId": owner_session_id, "runId": run_id,
            "externalTool": external,
            "sourceSessionId":session_id,
            "operationId": operation_id,
            "workspace": authority.map(|authority|authority.workspace.as_str()).unwrap_or(&desktop.workspace),
            "toolName": prepared.tool_name, "toolKind": desktop.tool_kinds.get(&prepared.tool_name), "input": prepared.parsed_args,
        });
        if params.to_string().len() > 2 * 1024 * 1024 {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Document tool request exceeds 2 MiB budget",
            ));
        }
        let request = acp::ExtRequest::new(
            "echo.agent/desktop_document_tool",
            serde_json::value::to_raw_value(&params)
                .map_err(|_| {
                    echo_agent_tool_runtime::ToolError::invalid_arguments(
                        "Invalid document tool request",
                    )
                })?
                .into(),
        );
        // A disconnected/replaced editor must fail closed. In particular a
        // timed-out write is never replayed through the disk tool fallback.
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            echo_agent_acp::acp_send(request, &desktop.gateway.tx()),
        )
        .await
        .map_err(|_| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Editor coordination timed out; operation was not replayed",
            )
        })?
        .map_err(|_| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Editor coordination unavailable; retry after reconnect",
            )
        })?;
        let result: serde_json::Value = serde_json::from_str(response.0.get()).map_err(|_| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Invalid editor coordination response",
            )
        })?;
        if let Some(error) = result.get("error").and_then(serde_json::Value::as_str) {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(error));
        }
        if result.get("handled").and_then(serde_json::Value::as_bool) == Some(true) {
            let text = result
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    echo_agent_tool_runtime::ToolError::invalid_arguments(
                        "Editor response omitted operation result",
                    )
                })?
                .to_string();
            return Ok(ToolRunResult {
                output: echo_agent_tools::types::output::ToolOutput::Text(
                    echo_agent_tools::types::output::TextOutput::from(text.clone()),
                ),
                prompt_text: text,
                effective_tool_name: None,
            });
        }
        // handled:false is an explicit host preflight approval for a tool the
        // host does not implement (e.g. terminal execution). Omitted/malformed
        // approval cannot silently bypass document protection.
        if result.get("handled").and_then(serde_json::Value::as_bool) != Some(false) {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Editor did not approve tool dispatch",
            ));
        }
    }
    let result = workspace_ops
        .call_tool(
            &prepared.tool_name,
            prepared.parsed_args.clone(),
            &prepared.tool_call_id.0,
            Some(session_id),
        )
        .await;
    if let Some(desktop) = desktop {
        let params = serde_json::json!({
            "phase": "after", "sessionId": owner_session_id, "runId": run_id,
            "sourceSessionId":session_id,
            "operationId": operation_id,
            "workspace": authority.map(|authority|authority.workspace.as_str()).unwrap_or(&desktop.workspace), "toolName": prepared.tool_name,
            "toolKind": desktop.tool_kinds.get(&prepared.tool_name), "input": prepared.parsed_args,
            "succeeded": result.is_ok(),
            "externalTool": external, "executionSettled": external && result.is_ok(),
        });
        let request = acp::ExtRequest::new(
            "echo.agent/desktop_document_tool",
            serde_json::value::to_raw_value(&params)
                .map_err(|_| {
                    echo_agent_tool_runtime::ToolError::invalid_arguments(
                        "Invalid post-tool observation",
                    )
                })?
                .into(),
        );
        // Commands may already have changed disk even when they fail. Always
        // observe after dispatch, and report an observation failure truthfully.
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            echo_agent_acp::acp_send(request, &desktop.gateway.tx()),
        )
        .await
        {
            Ok(Ok(response)) => {
                let observed: serde_json::Value =
                    serde_json::from_str(response.0.get()).map_err(|_| {
                        echo_agent_tool_runtime::ToolError::invalid_arguments(
                            "Tool finished, but change observation returned invalid data",
                        )
                    })?;
                if let Some(error) = observed.get("error").and_then(serde_json::Value::as_str) {
                    return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                        format!(
                            "Tool finished; existing side effects remain. Editor observation: {error}"
                        ),
                    ));
                }
            }
            _ => {
                return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                    "Tool finished, but editor change observation failed. Existing side effects remain; inspect disk before retrying.",
                ));
            }
        }
    }
    result
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

    fn desktop_fixture() -> (
        DesktopDocumentBridge,
        tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
        PreparedToolCall,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let desktop = DesktopDocumentBridge {
            gateway: echo_agent_acp::AcpAgentGatewaySender::new(tx),
            authority: None,
            run_id: "actual-prompt-id".to_string(),
            workspace: "/tmp/owned-workspace".to_string(),
            tool_kinds: [("write".to_string(), "write".to_string())].into(),
        };
        let prepared = PreparedToolCall {
            call_id: "model-call".into(),
            tool_call_id: acp::ToolCallId::new("operation-id"),
            tool_name: "write".into(),
            raw_arguments: r#"{"file_path":"a.txt","content":"new text"}"#.into(),
            parsed_args: serde_json::json!({"file_path":"a.txt","content":"new text"}),
            model_id: "test-model".into(),
            concatenated_json_count: 1,
            dispatch_target_name: None,
            is_read_only: false,
        };
        (desktop, rx, prepared)
    }

    fn desktop_reply(
        message: echo_agent_acp::AcpClientMessage,
        value: serde_json::Value,
    ) -> serde_json::Value {
        let echo_agent_acp::AcpClientMessage::ExtMethod(args) = message else {
            panic!("document dispatch must use the typed ACP extension channel")
        };
        assert_eq!(&*args.request.method, "echo.agent/desktop_document_tool");
        let params = serde_json::from_str(args.request.params.get()).unwrap();
        let raw = serde_json::value::to_raw_value(&value).unwrap();
        let _ = args.response_tx.send(Ok(acp::ExtResponse::new(raw.into())));
        params
    }

    #[tokio::test]
    async fn desktop_document_bridge_returns_editor_result_without_disk_fallback() {
        let ops = echo_agent_workspace::WorkspaceOps::for_test();
        let (desktop, mut rx, prepared) = desktop_fixture();
        let host = async {
            let params = desktop_reply(
                rx.recv().await.unwrap(),
                serde_json::json!({"handled":true,"text":"Applied editor version 8"}),
            );
            assert_eq!(params["phase"], "before");
            assert_eq!(params["sessionId"], "session-id");
            assert_eq!(params["runId"], "actual-prompt-id");
            assert_eq!(
                params["operationId"],
                "actual-prompt-id:session-id:operation-id"
            );
            assert_eq!(params["workspace"], "/tmp/owned-workspace");
            assert_eq!(params["toolKind"], "write");
            assert_eq!(params["input"], prepared.parsed_args);
        };
        let (result, ()) = tokio::join!(
            dispatch_tool(&ops, &prepared, "session-id", Some(&desktop)),
            host
        );
        // The workspace has no bound tool session. A disk fallback would fail.
        assert_eq!(result.unwrap().prompt_text, "Applied editor version 8");
        assert!(
            rx.try_recv().is_err(),
            "handled writes need no second execution"
        );
    }

    #[tokio::test]
    async fn desktop_document_bridge_child_dispatch_uses_parent_run_and_workspace() {
        let ops = echo_agent_workspace::WorkspaceOps::for_test();
        let (mut desktop, mut rx, prepared) = desktop_fixture();
        desktop.authority = Some(crate::session::InheritedTurnAuthority {
            prompt_mode: crate::session::plan_mode::PromptMode::Agent,
            document_tools: true,
            session_id: "parent-session".into(),
            run_id: "parent-turn".into(),
            workspace: "/tmp/parent-workspace".into(),
        });
        let host = async {
            let params = desktop_reply(
                rx.recv().await.unwrap(),
                serde_json::json!({"handled":true,"text":"Applied child document edit"}),
            );
            assert_eq!(params["sessionId"], "parent-session");
            assert_eq!(params["sourceSessionId"], "child-session");
            assert_eq!(params["runId"], "parent-turn");
            assert_eq!(params["workspace"], "/tmp/parent-workspace");
            assert_eq!(
                params["operationId"],
                "parent-turn:child-session:operation-id"
            );
        };
        let (result, ()) = tokio::join!(
            dispatch_tool(&ops, &prepared, "child-session", Some(&desktop)),
            host
        );
        assert_eq!(result.unwrap().prompt_text, "Applied child document edit");
    }

    #[tokio::test]
    async fn desktop_document_bridge_requires_explicit_valid_preflight() {
        let ops = echo_agent_workspace::WorkspaceOps::for_test();
        for response in [
            serde_json::json!({"error":"Document version conflict"}),
            serde_json::json!({}),
            serde_json::json!({"handled":"false"}),
            serde_json::json!({"handled":true}),
            serde_json::json!(null),
        ] {
            let (desktop, mut rx, prepared) = desktop_fixture();
            let host = async { desktop_reply(rx.recv().await.unwrap(), response) };
            let (result, _) = tokio::join!(
                dispatch_tool(&ops, &prepared, "session-id", Some(&desktop)),
                host
            );
            assert!(result.is_err());
            assert!(
                rx.try_recv().is_err(),
                "invalid approval must not execute disk tools"
            );
        }
        let (desktop, rx, prepared) = desktop_fixture();
        drop(rx);
        let result = dispatch_tool(&ops, &prepared, "session-id", Some(&desktop)).await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("coordination unavailable")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn desktop_document_bridge_timeout_and_input_budget_fail_closed() {
        let ops = echo_agent_workspace::WorkspaceOps::for_test();
        let (desktop, mut rx, prepared) = desktop_fixture();
        let host = async {
            let message = rx.recv().await.unwrap();
            // Hold the response channel open while virtual time advances.
            tokio::time::sleep(std::time::Duration::from_secs(31)).await;
            drop(message);
        };
        let (result, ()) = tokio::join!(
            dispatch_tool(&ops, &prepared, "session-id", Some(&desktop)),
            host
        );
        assert!(result.unwrap_err().to_string().contains("not replayed"));
        assert!(rx.try_recv().is_err());

        let (desktop, mut rx, mut prepared) = desktop_fixture();
        prepared.parsed_args = serde_json::json!({"content":"x".repeat(2 * 1024 * 1024)});
        let result = dispatch_tool(&ops, &prepared, "session-id", Some(&desktop)).await;
        assert!(result.unwrap_err().to_string().contains("2 MiB"));
        assert!(
            rx.try_recv().is_err(),
            "oversized inputs must not reach the host"
        );
    }

    #[tokio::test]
    async fn desktop_document_bridge_observes_failed_fallback_and_keeps_identity() {
        let ops = echo_agent_workspace::WorkspaceOps::for_test();
        let (desktop, mut rx, prepared) = desktop_fixture();
        let host = async {
            let before = desktop_reply(
                rx.recv().await.unwrap(),
                serde_json::json!({"handled":false}),
            );
            let after = desktop_reply(
                rx.recv().await.unwrap(),
                serde_json::json!({"handled":true}),
            );
            assert_eq!(before["phase"], "before");
            assert_eq!(after["phase"], "after");
            assert_eq!(after["succeeded"], false);
            for field in [
                "sessionId",
                "runId",
                "operationId",
                "workspace",
                "toolName",
                "toolKind",
                "input",
            ] {
                assert_eq!(before[field], after[field]);
            }
        };
        let future = dispatch_tool(&ops, &prepared, "session-id", Some(&desktop));
        fn require_send<T: Send>(_: &T) {}
        require_send(&future);
        let (result, ()) = tokio::join!(future, host);
        assert!(
            result.is_err(),
            "unbound workspace tool failure must remain failed"
        );
    }

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
