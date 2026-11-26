//! Single source of truth for the `sessionUpdate` discriminant strings the
//! session-resume replay matchers compare against persisted `updates.jsonl` lines.
//!
//! Each value is derived from its enum's serde impl (not a hand-written literal),
//! so renaming a variant updates the matcher automatically. The guard test pins
//! the wire format, turning an accidental serde change into a failing test.

use std::sync::LazyLock;

use agent_client_protocol as acp;
use echo_agent_tools::types::TaskSnapshot;

use crate::extensions::notification::SessionUpdate as EchoAgentSessionUpdate;

/// Serialize an internally-tagged session-update value and return the
/// `sessionUpdate` discriminant serde itself emits for that variant.
fn tagged_discriminant<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| {
            v.get("sessionUpdate")
                .and_then(|t| t.as_str())
                .map(str::to_owned)
        })
        .expect("internally-tagged session-update value must serialize with a sessionUpdate tag")
}

/// `acp::SessionUpdate::UserMessageChunk` discriminant.
pub(crate) static USER_MESSAGE_CHUNK: LazyLock<String> = LazyLock::new(|| {
    tagged_discriminant(&acp::SessionUpdate::UserMessageChunk(
        acp::ContentChunk::new(acp::ContentBlock::from("")),
    ))
});

/// `acp::SessionUpdate::AvailableCommandsUpdate` discriminant.
pub(crate) static AVAILABLE_COMMANDS_UPDATE: LazyLock<String> = LazyLock::new(|| {
    tagged_discriminant(&acp::SessionUpdate::AvailableCommandsUpdate(
        acp::AvailableCommandsUpdate::new(Vec::new()),
    ))
});

/// `acp::SessionUpdate::ToolCallUpdate` discriminant.
pub(crate) static TOOL_CALL_UPDATE: LazyLock<String> = LazyLock::new(|| {
    tagged_discriminant(&acp::SessionUpdate::ToolCallUpdate(
        acp::ToolCallUpdate::new(acp::ToolCallId::new("t"), acp::ToolCallUpdateFields::new()),
    ))
});

/// `acp::ToolCallStatus::InProgress` wire string (`in_progress`).
pub(crate) static TOOL_CALL_STATUS_IN_PROGRESS: LazyLock<String> = LazyLock::new(|| {
    serde_json::to_value(acp::ToolCallStatus::InProgress)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .expect("ToolCallStatus::InProgress must serialize as a string")
});

/// EchoAgent `SessionUpdate::RewindMarker` discriminant. Appears verbatim in compact
/// JSON, so it doubles as a cheap substring pre-filter.
pub(crate) static REWIND_MARKER: LazyLock<String> = LazyLock::new(|| {
    tagged_discriminant(&EchoAgentSessionUpdate::RewindMarker {
        target_prompt_index: 0,
        created_at: String::new(),
    })
});

/// EchoAgent `SessionUpdate::TaskBackgrounded` discriminant.
pub(crate) static TASK_BACKGROUNDED: LazyLock<String> = LazyLock::new(|| {
    tagged_discriminant(&EchoAgentSessionUpdate::TaskBackgrounded {
        tool_call_id: String::new(),
        task_id: String::new(),
        command: String::new(),
        cwd: String::new(),
        output_file: String::new(),
        monitor_description: None,
        description: None,
    })
});

/// EchoAgent `SessionUpdate::TaskCompleted` discriminant.
pub(crate) static TASK_COMPLETED: LazyLock<String> = LazyLock::new(|| {
    tagged_discriminant(&EchoAgentSessionUpdate::TaskCompleted {
        task_snapshot: TaskSnapshot {
            task_id: String::new(),
            command: String::new(),
            display_command: None,
            cwd: String::new(),
            start_time: std::time::SystemTime::UNIX_EPOCH,
            end_time: None,
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: None,
            signal: None,
            completed: false,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        },
        will_wake: false,
    })
});

/// The `"sessionUpdate":"user_message_chunk"` key/value pair as serialized (no
/// leading `{`). Built from [`USER_MESSAGE_CHUNK`] so the literal isn't hand-
/// maintained; the quoted key means it can't false-match the bare discriminant
/// escaped inside user content.
pub(crate) static USER_MESSAGE_CHUNK_PREFIX: LazyLock<String> =
    LazyLock::new(|| format!(r#""sessionUpdate":"{}""#, *USER_MESSAGE_CHUNK));

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the wire format: each derived discriminant must equal the exact
    /// string persisted in `updates.jsonl`, so a serde/wire change fails here.
    #[test]
    fn derived_discriminants_match_wire_format() {
        assert_eq!(USER_MESSAGE_CHUNK.as_str(), "user_message_chunk");
        assert_eq!(
            AVAILABLE_COMMANDS_UPDATE.as_str(),
            "available_commands_update"
        );
        assert_eq!(TOOL_CALL_UPDATE.as_str(), "tool_call_update");
        assert_eq!(TOOL_CALL_STATUS_IN_PROGRESS.as_str(), "in_progress");
        assert_eq!(REWIND_MARKER.as_str(), "rewind_marker");
        assert_eq!(TASK_BACKGROUNDED.as_str(), "task_backgrounded");
        assert_eq!(TASK_COMPLETED.as_str(), "task_completed");
        assert_eq!(
            USER_MESSAGE_CHUNK_PREFIX.as_str(),
            r#""sessionUpdate":"user_message_chunk""#
        );
    }
}
