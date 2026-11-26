//! Extracting searchable text from session update files.
//!
//! The peek structs are shared with the resume/replay collectors in
//! [`super`], so the indexed text cannot drift from what a resumed session
//! replays. Everything downstream of the extracted string (hashing, dedup,
//! the SQLite index itself) lives in `echo-agent-session-search`.

use std::io::{self, BufRead};
use std::path::Path;

use super::{
    ContentPeek, ECHO_AGENT_SESSION_UPDATE_METHOD, PromptExtractEvent, RawLinePeek, RawParamsPeek,
    collect_prompts_from_events,
};
use crate::session::wire_tags::{REWIND_MARKER, USER_MESSAGE_CHUNK};

const SEARCH_CONTENT_CHAR_LIMIT: usize = 200_000;

// Zero-copy peek structs. Text-bearing fields are `Cow`, not `&str`: serde
// cannot borrow `&str` from JSON strings containing escapes, so borrowing
// would error and silently drop the message from the index.

/// Peek for assistant text (agent_message_chunk content.text).
#[derive(serde::Deserialize)]
struct AgentContentPeek<'a> {
    #[serde(borrow)]
    update: AgentUpdatePeek<'a>,
}

#[derive(serde::Deserialize)]
struct AgentUpdatePeek<'a> {
    #[serde(borrow, default)]
    content: Option<AgentTextPeek<'a>>,
}

#[derive(serde::Deserialize)]
struct AgentTextPeek<'a> {
    #[serde(rename = "type", default)]
    content_type: Option<&'a str>,
    #[serde(borrow, default)]
    text: Option<std::borrow::Cow<'a, str>>,
}

/// Peek for user message content (user_message_chunk content.text). Reuses
/// [`ContentPeek`] so the peeked fields stay single-sourced.
#[derive(serde::Deserialize)]
struct UserContentPeek<'a> {
    #[serde(borrow)]
    update: UserUpdatePeek<'a>,
}

#[derive(serde::Deserialize)]
struct UserUpdatePeek<'a> {
    #[serde(borrow, default)]
    content: Option<ContentPeek<'a>>,
    #[serde(default, rename = "_meta")]
    meta: Option<super::RawChunkMetaPeek>,
}

/// Peek for tool call metadata (tool_call title + locations[].path).
#[derive(serde::Deserialize)]
struct ToolCallPeek<'a> {
    #[serde(borrow)]
    update: ToolUpdatePeek<'a>,
}

#[derive(serde::Deserialize)]
struct ToolUpdatePeek<'a> {
    #[serde(borrow, default)]
    title: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default)]
    locations: Option<Vec<ToolLocationPeek<'a>>>,
}

#[derive(serde::Deserialize)]
struct ToolLocationPeek<'a> {
    #[serde(borrow, default)]
    path: Option<std::borrow::Cow<'a, str>>,
}

/// Collect all indexable content from a session's `updates.jsonl` in one
/// pass, without materializing full `acp::SessionNotification` objects.
pub(super) fn collect_all_indexable_content_single_pass(
    updates_path: &Path,
) -> io::Result<(String, u64)> {
    let file = match std::fs::File::open(updates_path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((String::new(), 0)),
        Err(e) => return Err(e),
    };
    let bytes_read = file.metadata().map(|m| m.len()).unwrap_or(0);
    let reader = io::BufReader::new(file);

    let mut prompt_events: Vec<PromptExtractEvent> = Vec::new();
    let mut assistant_texts: Vec<String> = Vec::new();
    let mut current_assistant: String = String::new();
    let mut tool_meta: Vec<String> = Vec::new();
    let mut assistant_chars = 0usize;
    let mut tool_call_count = 0usize;
    let mut tool_chars_emitted = 0usize;

    const ASSISTANT_MAX_CHARS: usize = 100_000;
    const TOOL_MAX_CALLS: usize = 200;
    const TOOL_MAX_CHARS: usize = 100_000;

    // Flush the assistant buffer on turn boundary; called in every
    // non-agent_message_chunk branch to match `collect_assistant_text`.
    let flush_assistant = |current: &mut String, texts: &mut Vec<String>| {
        if !current.is_empty() {
            let t = current.trim().to_string();
            if !t.is_empty() {
                texts.push(t);
            }
            current.clear();
        }
    };

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "skipping unreadable line in single-pass content collector");
                // An I/O error is a turn boundary, matching the
                // iterator-based collectors.
                flush_assistant(&mut current_assistant, &mut assistant_texts);
                prompt_events.push(PromptExtractEvent::NotUserMessage);
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let (raw_params, is_echo_agent) =
            if let Ok(env) = serde_json::from_str::<RawLinePeek<'_>>(trimmed) {
                let raw = env.params.map(|p| p.get()).unwrap_or(trimmed);
                let echoagent = env.method == Some(ECHO_AGENT_SESSION_UPDATE_METHOD);
                (raw, echoagent)
            } else {
                (trimmed, false)
            };

        let update_peek = serde_json::from_str::<RawParamsPeek<'_>>(raw_params)
            .ok()
            .and_then(|p| p.update);
        let tag = update_peek.as_ref().map(|u| u.session_update);

        // Content events arrive on ACP "session/update"; control events
        // (rewind markers) on the EchoAgent "_echo.agent/session/update" extension.
        if !is_echo_agent {
            match tag {
                Some(t) if t == *USER_MESSAGE_CHUNK => {
                    flush_assistant(&mut current_assistant, &mut assistant_texts);
                    if let Ok(peek) = serde_json::from_str::<UserContentPeek<'_>>(raw_params)
                        && let Some(content) = peek.update.content
                        && content.content_type == Some("text")
                        && let Some(text) = content.text
                    {
                        if content
                            .meta
                            .as_ref()
                            .is_some_and(|m| m.bash_command.is_some())
                            || peek
                                .update
                                .meta
                                .as_ref()
                                .is_some_and(|m| m.host_turn == Some(true))
                        {
                            prompt_events.push(PromptExtractEvent::NotUserMessage);
                        } else {
                            let prompt_index = peek
                                .update
                                .meta
                                .as_ref()
                                .and_then(|m| m.prompt_index.map(|v| v as usize));
                            prompt_events.push(PromptExtractEvent::UserTextChunk {
                                text: text.into_owned(),
                                prompt_index,
                            });
                        }
                    } else {
                        prompt_events.push(PromptExtractEvent::NotUserMessage);
                    }
                }
                Some("agent_message_chunk") => {
                    // Same assistant turn: no flush.
                    if assistant_chars < ASSISTANT_MAX_CHARS
                        && let Ok(peek) = serde_json::from_str::<AgentContentPeek<'_>>(raw_params)
                        && let Some(content) = peek.update.content
                        && content.content_type == Some("text")
                        && let Some(text) = content.text
                        && !text.is_empty()
                    {
                        let sep_cost = usize::from(!current_assistant.is_empty());
                        let budget = ASSISTANT_MAX_CHARS
                            .saturating_sub(assistant_chars)
                            .saturating_sub(sep_cost);
                        if budget > 0 {
                            if sep_cost > 0 {
                                current_assistant.push(' ');
                                assistant_chars += 1;
                            }
                            let mut take = text.len().min(budget);
                            while take > 0 && !text.is_char_boundary(take) {
                                take -= 1;
                            }
                            current_assistant.push_str(&text[..take]);
                            assistant_chars += take;
                        }
                    }
                    prompt_events.push(PromptExtractEvent::NotUserMessage);
                }
                Some("agent_thought_chunk") => {
                    // Same assistant turn: not indexed, but must not flush.
                    prompt_events.push(PromptExtractEvent::NotUserMessage);
                }
                Some("tool_call") => {
                    flush_assistant(&mut current_assistant, &mut assistant_texts);
                    if tool_call_count < TOOL_MAX_CALLS {
                        tool_call_count += 1;
                        if let Ok(peek) = serde_json::from_str::<ToolCallPeek<'_>>(raw_params) {
                            if let Some(title) = peek.update.title
                                && !title.is_empty()
                            {
                                let budget = TOOL_MAX_CHARS.saturating_sub(tool_chars_emitted);
                                if budget > 0 {
                                    let mut take = title.len().min(budget);
                                    while take > 0 && !title.is_char_boundary(take) {
                                        take -= 1;
                                    }
                                    tool_meta.push(title[..take].to_string());
                                    tool_chars_emitted += take;
                                }
                            }
                            if let Some(locs) = peek.update.locations {
                                for loc in locs {
                                    if let Some(p) = loc.path
                                        && !p.is_empty()
                                    {
                                        let budget =
                                            TOOL_MAX_CHARS.saturating_sub(tool_chars_emitted);
                                        if budget > 0 {
                                            let mut take = p.len().min(budget);
                                            while take > 0 && !p.is_char_boundary(take) {
                                                take -= 1;
                                            }
                                            tool_meta.push(p[..take].to_string());
                                            tool_chars_emitted += take;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    prompt_events.push(PromptExtractEvent::NotUserMessage);
                }
                _ => {
                    flush_assistant(&mut current_assistant, &mut assistant_texts);
                    prompt_events.push(PromptExtractEvent::NotUserMessage);
                }
            }
        } else {
            match tag {
                Some(t) if t == *REWIND_MARKER => {
                    flush_assistant(&mut current_assistant, &mut assistant_texts);
                    if let Some(ref u) = update_peek
                        && let Some(idx) = u.target_prompt_index
                    {
                        prompt_events.push(PromptExtractEvent::RewindTo(idx));
                    } else {
                        prompt_events.push(PromptExtractEvent::NotUserMessage);
                    }
                }
                _ => {
                    flush_assistant(&mut current_assistant, &mut assistant_texts);
                    prompt_events.push(PromptExtractEvent::NotUserMessage);
                }
            }
        }
    }

    if !current_assistant.is_empty() {
        let t = current_assistant.trim().to_string();
        if !t.is_empty() {
            assistant_texts.push(t);
        }
    }

    let prompts = collect_prompts_from_events(prompt_events.into_iter());

    let parts = [
        prompts.join("\n\n"),
        assistant_texts.join("\n"),
        tool_meta.join("\n"),
    ];
    let mut joined = parts.join("\n\n");

    if joined.len() > SEARCH_CONTENT_CHAR_LIMIT {
        // Keep the tail: the most recent content is the most relevant.
        let mut start = joined.len().saturating_sub(SEARCH_CONTENT_CHAR_LIMIT);
        while start < joined.len() && !joined.is_char_boundary(start) {
            start += 1;
        }
        joined = joined[start..].to_string();
    }

    Ok((joined, bytes_read))
}

/// Summary fixture shared by this module's tests and `search.rs`.
#[cfg(test)]
pub(super) fn test_summary(
    session_id: &str,
    cwd: &str,
    title: &str,
) -> crate::session::persistence::Summary {
    use crate::session::info::Info;
    use crate::session::persistence::Summary;
    use agent_client_protocol as acp;

    Summary {
        info: Info {
            id: acp::SessionId::new(session_id),
            cwd: cwd.to_string(),
        },
        cwd_generation: 0,
        previous_cwd: None,
        pending_cwd_switch_reminder: None,
        cwd_switch_bookkeeping_generation: 0,
        session_summary: title.to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        num_messages: 0,
        num_chat_messages: 0,
        current_model_id: acp::ModelId::new("test"),
        parent_session_id: None,
        forked_at: None,
        collection_id: None,
        next_trace_turn: 0,
        chat_format_version: 1,
        prompt_display_cwd: None,
        session_kind: None,
        fork_context_source: None,
        fork_parent_prompt_id: None,
        inherited_prefix_len: None,
        title_source_prompt_index: None,
        hidden: None,
        source_workspace_dir: None,
        git_root_dir: None,
        git_remotes: Vec::new(),
        head_commit: None,
        head_branch: None,
        request_id: None,
        echo_agent_home: None,
        last_active_at: None,
        generated_title: None,
        title_is_manual: false,
        worktree_label: None,
        agent_name: None,
        sandbox_profile: None,
        reasoning_effort: None,
        last_turn_summary: None,
        last_turn_summary_prompt_id: None,
        last_recap: None,
    }
}

#[cfg(test)]
#[path = "search_content_tests.rs"]
mod tests;
