//! Session forking functionality
//!
//! Forks a saved session to a new working directory with a new session ID.
//! This creates new session files but does not start the session.

use crate::remote::BackendClient;
const FORK_LOG: &str = "echo_agent_fork";
use crate::session::export::ExportedMetadata;
use crate::session::info::Info;
use crate::session::storage::{
    CopySessionOptions, JsonlStorageAdapter, SessionUpdate, StorageAdapter, UpdatesIterator,
};
use crate::util::echo_agent_home::echo_agent_home;
use agent_client_protocol as acp;
use std::io;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSessionRequest {
    pub source_session_id: String,
    pub source_cwd: String,
    pub new_cwd: String,
    /// Client-provided session ID for the forked session.
    /// If None, a new ID will be auto-generated.
    #[serde(default)]
    pub new_session_id: Option<String>,
    /// Optional model ID override for the forked session.
    /// If None, the source session's model will be used.
    #[serde(default)]
    pub new_model_id: Option<String>,
    #[serde(default)]
    pub target_prompt_index: Option<usize>,
    /// Desktop continuations start fresh execution state and generate their own title.
    #[serde(default)]
    pub fresh_continuation: bool,
    /// Stable identity of the selected source turn, checked after persistence flush.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_prompt_id: Option<String>,
    /// Override `session_kind` in the forked summary. Defaults to `"fork"`.
    /// Worktree forks set this to `"worktree"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
    /// The original workspace directory this worktree session was spawned from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_workspace_dir: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSessionResponse {
    pub new_session_id: String,
    pub chat_messages_copied: usize,
    pub updates_copied: usize,
    pub plan_state_copied: bool,
    /// The working directory of the new forked session
    pub new_cwd: String,
    /// The parent session ID (source session that was forked)
    pub parent_session_id: String,
    /// The model ID of the forked session (may differ from source if overridden)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_model_id: Option<String>,
}

/// Generate a forked session ID.
///
/// Uses a plain UUIDv7 -- no prefix or source embedding. This keeps IDs
/// a constant 36 chars regardless of how many fork rounds occur.
fn generate_fork_session_id(_source_id: &str) -> String {
    uuid::Uuid::now_v7().to_string()
}

fn prompt_id_matches(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    expected: &str,
) -> bool {
    meta.is_some_and(|meta| {
        ["promptId", "prompt_id"]
            .iter()
            .any(|key| meta.get(*key).and_then(serde_json::Value::as_str) == Some(expected))
    })
}

/// Inspect only the selected live turn. Rewind markers can reuse an index for
/// different content, so finding an old prompt id anywhere in the log is unsafe.
/// Keep constant memory even for sessions with large tool outputs or many turns.
pub fn source_prompt_matches(
    updates: impl Iterator<Item = std::io::Result<SessionUpdate>>,
    target: usize,
    expected: &str,
) -> std::io::Result<bool> {
    let target_turn = target
        .checked_add(1)
        .ok_or_else(|| io::Error::other("历史轮次超出范围"))?;
    let mut turns = 0usize;
    let mut seen_marker = false;
    let mut in_user = false;
    let mut current_index = None;
    let mut found = false;
    for update in updates {
        let update = match update {
            Ok(update) => update,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                in_user = false;
                continue;
            }
            Err(error) => return Err(error),
        };
        match update {
            SessionUpdate::EchoAgent(notification) => {
                in_user = false;
                if let crate::extensions::notification::SessionUpdate::RewindMarker {
                    target_prompt_index,
                    ..
                } = notification.update
                {
                    turns = turns.min(target_prompt_index);
                    if target_prompt_index <= target {
                        found = false;
                    }
                } else if turns == target_turn {
                    found |= prompt_id_matches(
                        notification
                            .meta
                            .as_ref()
                            .and_then(serde_json::Value::as_object),
                        expected,
                    );
                }
            }
            SessionUpdate::Acp(notification) => {
                let chunk_meta = match &notification.update {
                    agent_client_protocol::SessionUpdate::UserMessageChunk(chunk)
                    | agent_client_protocol::SessionUpdate::AgentMessageChunk(chunk)
                    | agent_client_protocol::SessionUpdate::AgentThoughtChunk(chunk) => {
                        chunk.meta.as_ref()
                    }
                    _ => None,
                };
                let is_user = matches!(
                    notification.update,
                    agent_client_protocol::SessionUpdate::UserMessageChunk(_)
                ) && !chunk_meta.is_some_and(|meta| {
                    meta.get("hostTurn").and_then(serde_json::Value::as_bool) == Some(true)
                });
                if is_user {
                    let index = chunk_meta
                        .and_then(|meta| meta.get("promptIndex"))
                        .and_then(serde_json::Value::as_u64);
                    seen_marker |= index.is_some();
                    let new_run = !in_user || seen_marker && index != current_index;
                    if new_run && (!seen_marker || index.is_some()) {
                        turns += 1;
                    }
                    in_user = true;
                    current_index = index;
                } else {
                    in_user = false;
                }
                if turns == target_turn {
                    found |= prompt_id_matches(notification.meta.as_ref(), expected)
                        || prompt_id_matches(chunk_meta, expected);
                }
            }
        }
    }
    Ok(found)
}

/// Fork a saved session to a new working directory.
pub async fn fork_session(
    request: ForkSessionRequest,
    agent_id: &str,
    auth_manager: Option<std::sync::Arc<crate::auth::AuthManager>>,
) -> io::Result<ForkSessionResponse> {
    let t0 = std::time::Instant::now();

    let root_dir = echo_agent_home();
    let storage = JsonlStorageAdapter::with_root(root_dir.clone());

    // Build source and target Info
    let source_info = Info {
        id: acp::SessionId::new(request.source_session_id.clone()),
        cwd: request.source_cwd.clone(),
    };

    // Use client-provided session ID or generate one
    let new_session_id = request
        .new_session_id
        .clone()
        .unwrap_or_else(|| generate_fork_session_id(&request.source_session_id));

    let target_info = Info {
        id: acp::SessionId::new(new_session_id.clone()),
        cwd: request.new_cwd.clone(),
    };

    // Copy session data with parent tracking.
    // Runs on the blocking thread pool so concurrent fork copies can execute
    // truly in parallel (on a LocalSet, async copy_session_data serializes
    // because the sync disk I/O blocks the single-threaded runtime).
    let options = CopySessionOptions {
        parent_session_id: Some(request.source_session_id.clone()),
        new_model_id: request.new_model_id.clone(),
        target_prompt_index: request.target_prompt_index,
        fresh_title: request.fresh_continuation,
        copy_plan_state: !request.fresh_continuation,
        copy_plan_mode_state: !request.fresh_continuation,
        copy_signals: !request.fresh_continuation,
        copy_tool_state: !request.fresh_continuation,
        copy_announcement_state: !request.fresh_continuation,
        session_kind: request.session_kind.clone(),
        source_workspace_dir: request.source_workspace_dir.clone(),
        // Carry the parent's compaction segment archive into the fork so the
        // child retains pre-compaction history (the live summary is already
        // copied via chat_history.jsonl).
        // Raw archives may contain later turns than a desktop history cut.
        // Its retained replay and checkpoints already preserve the chosen context.
        copy_compaction_segments: !request.fresh_continuation,
        ..Default::default()
    };

    let expected_prompt_id = request
        .source_prompt_id
        .clone()
        .filter(|_| request.fresh_continuation);
    let result = tokio::task::spawn_blocking(move || {
        if let Some(expected) = expected_prompt_id {
            let target = options
                .target_prompt_index
                .ok_or_else(|| io::Error::other("接续请求缺少历史轮次"))?;
            let path = storage
                .updates_file_path(&source_info)
                .ok_or_else(|| io::Error::other("无法核对所选历史轮次"))?;
            let updates = UpdatesIterator::open(&path)?
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "原对话历史不存在"))?;
            if !source_prompt_matches(updates, target, &expected)? {
                return Err(io::Error::other(
                    "该历史轮次已被回溯或替换，请重新加载原对话后选择接续位置",
                ));
            }
        }
        storage.copy_session_data_sync(&source_info, &target_info, options)
    })
    .await
    .map_err(|e| io::Error::other(format!("spawn_blocking panicked: {e}")))??;

    let copy_ms = t0.elapsed().as_millis() as u64;

    // Writeback session to backend (fire-and-forget).
    // This is telemetry-grade: the local fork works without it. All fork
    // state lives locally (session files on disk), and the caller does not
    // depend on synchronous backend registration. The backend eventually
    // learns about the session when the background task completes.
    // Spawning removes the network round-trip (~200-400ms) from the
    // critical path.
    if let Some(am) = auth_manager {
        let sid = new_session_id.clone();
        let cwd = request.new_cwd.clone();
        let parent = request.source_session_id.clone();
        let model = request.new_model_id.clone();
        let aid = agent_id.to_string();
        tokio::spawn(async move {
            if let Err(e) =
                sync_forked_session_to_backend(&sid, &cwd, parent, model, &aid, am).await
            {
                tracing::warn!(
                    session_id = %sid,
                    error = %e,
                    "Failed to register forked session with backend (background)"
                );
            }
        });
    }

    let total_ms = t0.elapsed().as_millis() as u64;
    tracing::info!(
        target: FORK_LOG,
        session_id = %new_session_id,
        source_session = %request.source_session_id,
        copy_ms,
        total_ms,
        chat_copied = result.chat_messages_copied,
        updates_copied = result.updates_copied,
        "FORK_COPY: session data copied (backend sync spawned in background)"
    );

    Ok(ForkSessionResponse {
        new_session_id,
        chat_messages_copied: result.chat_messages_copied,
        updates_copied: result.updates_copied,
        plan_state_copied: result.plan_state_copied,
        new_cwd: request.new_cwd,
        parent_session_id: request.source_session_id,
        new_model_id: request.new_model_id,
    })
}

/// Sync a forked session to the backend (for writeback mode).
async fn sync_forked_session_to_backend(
    session_id: &str,
    cwd: &str,
    parent_session_id: String,
    model_id: Option<String>,
    agent_id: &str,
    auth_manager: std::sync::Arc<crate::auth::AuthManager>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = BackendClient::new().with_auth_manager(auth_manager);
    let metadata = ExportedMetadata {
        title: None, // Will be generated later when session runs
        cwd: cwd.to_string(),
        model_id,
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        total_messages: Some(0),
        parent_session_id: Some(parent_session_id),
        session_kind: None,
        subagent_type: None,
        subagent_persona: None,
        subagent_role: None,
        fork_context_source: None,
        subagent_depth: None,
        title_is_manual: None,
    };

    client
        .upsert_session(session_id, &metadata, agent_id)
        .await?;
    tracing::info!(
        session_id = %session_id,
        "Forked session registered with backend"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_fork_session_id_format() {
        let fork_id = generate_fork_session_id("abc123");

        // Should be a valid UUIDv7 (36 chars with dashes)
        assert_eq!(
            fork_id.len(),
            36,
            "Fork ID should be a standard UUID length"
        );
        assert!(
            uuid::Uuid::parse_str(&fork_id).is_ok(),
            "Fork ID should be a valid UUID"
        );
    }

    #[test]
    fn test_generate_fork_session_id_uniqueness() {
        // Generate multiple IDs rapidly and ensure they're all unique
        let mut ids = std::collections::HashSet::new();
        for _ in 0..100 {
            let fork_id = generate_fork_session_id("any-source");
            assert_eq!(fork_id.len(), 36);
            ids.insert(fork_id);
        }

        // All 100 should be unique
        assert_eq!(ids.len(), 100, "All generated IDs should be unique");
    }

    #[test]
    fn test_generate_fork_session_id_constant_length() {
        // Forking from already-forked sessions should produce same-length IDs
        let id1 = generate_fork_session_id("019c43b5-c4ae-7190-b058-693e24669ba9");
        let id2 = generate_fork_session_id(&id1); // fork of fork
        let id3 = generate_fork_session_id(&id2); // fork of fork of fork

        assert_eq!(id1.len(), 36);
        assert_eq!(id2.len(), 36);
        assert_eq!(id3.len(), 36);
    }

    #[test]
    fn test_fork_session_request_serialization() {
        let request = ForkSessionRequest {
            source_session_id: "abc123".to_string(),
            source_cwd: "/old/project".to_string(),
            new_cwd: "/new/project".to_string(),
            new_session_id: Some("custom-session-id".to_string()),
            new_model_id: Some("echo-agent-3".to_string()),
            target_prompt_index: None,
            ..Default::default()
        };

        let json = serde_json::to_string(&request).unwrap();
        let deserialized: ForkSessionRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.source_session_id, "abc123");
        assert_eq!(deserialized.source_cwd, "/old/project");
        assert_eq!(deserialized.new_cwd, "/new/project");
        assert_eq!(
            deserialized.new_session_id,
            Some("custom-session-id".to_string())
        );
        assert_eq!(deserialized.new_model_id, Some("echo-agent-3".to_string()));
    }

    #[test]
    fn test_fork_session_request_without_optional_fields() {
        // Test that optional fields default to None when not provided
        let json = r#"{"sourceSessionId":"abc123","sourceCwd":"/old","newCwd":"/new"}"#;
        let deserialized: ForkSessionRequest = serde_json::from_str(json).unwrap();

        assert_eq!(deserialized.source_session_id, "abc123");
        assert_eq!(deserialized.new_session_id, None);
        assert_eq!(deserialized.new_model_id, None);
    }

    #[test]
    fn test_fork_session_response_serialization() {
        let response = ForkSessionResponse {
            new_session_id: "fork-abc123-12345678".to_string(),
            chat_messages_copied: 42,
            updates_copied: 156,
            plan_state_copied: true,
            new_cwd: "/new/project".to_string(),
            parent_session_id: "abc123".to_string(),
            new_model_id: Some("echo-agent-3".to_string()),
        };

        let json = serde_json::to_string(&response).unwrap();
        let deserialized: ForkSessionResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.new_session_id, "fork-abc123-12345678");
        assert_eq!(deserialized.chat_messages_copied, 42);
        assert_eq!(deserialized.updates_copied, 156);
        assert!(deserialized.plan_state_copied);
        assert_eq!(deserialized.new_cwd, "/new/project");
        assert_eq!(deserialized.parent_session_id, "abc123");
        assert_eq!(deserialized.new_model_id, Some("echo-agent-3".to_string()));
    }

    #[test]
    fn test_fork_session_response_without_model_override() {
        let response = ForkSessionResponse {
            new_session_id: "fork-abc123-12345678".to_string(),
            chat_messages_copied: 42,
            updates_copied: 156,
            plan_state_copied: true,
            new_cwd: "/new/project".to_string(),
            parent_session_id: "abc123".to_string(),
            new_model_id: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        // new_model_id should not be present in JSON when None
        assert!(!json.contains("new_model_id"));
    }
}
