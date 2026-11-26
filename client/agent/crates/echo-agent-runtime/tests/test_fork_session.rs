//! Integration tests for the fork session flow.
//!
//! These tests verify the complete fork session flow:
//! 1. Fork session data with parent tracking
//! 2. Verify forked session has correct metadata
//! 3. Test worktree creation from worktree types

use agent_client_protocol as acp;
use echo_agent_runtime::sampling::ConversationItem;
use echo_agent_runtime::session::info::Info;
use echo_agent_runtime::session::storage::{JsonlStorageAdapter, StorageAdapter};
use tempfile::TempDir;

/// Helper to create a test session in a temp directory
async fn create_test_session(storage: &JsonlStorageAdapter, session_id: &str, cwd: &str) -> Info {
    let info = Info {
        id: acp::SessionId::new(session_id),
        cwd: cwd.to_string(),
    };

    let model_id = acp::ModelId::new("echo-agent-code-fast-1");
    storage.init_session(&info, model_id).await.unwrap();

    // Add some chat messages
    let msg = ConversationItem::user("Hello world");
    storage.append_chat_message(&info, &msg).await.unwrap();

    // Add an update
    let notification = acp::SessionNotification::new(
        acp::SessionId::new(session_id),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new("Test response".to_string()),
        ))),
    );
    storage
        .append_update(
            &info,
            &echo_agent_runtime::session::storage::SessionUpdate::Acp(Box::new(notification)),
        )
        .await
        .unwrap();

    info
}

#[tokio::test]
async fn test_fork_session_creates_new_session_with_parent_tracking() {
    let temp_dir = TempDir::new().unwrap();
    let storage = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    // Create source session
    let source_info = create_test_session(&storage, "source-session-123", "/source/path").await;

    let target_info = Info {
        id: acp::SessionId::new("fork-session-456"),
        cwd: "/new/path".to_string(),
    };

    let options = echo_agent_runtime::session::storage::CopySessionOptions {
        parent_session_id: Some("source-session-123".to_string()),
        new_model_id: Some("echo-agent-3".to_string()),
        target_prompt_index: None,
        ..Default::default()
    };

    let result = storage
        .copy_session_data(&source_info, &target_info, options)
        .await
        .unwrap();

    // Verify result
    assert_eq!(result.chat_messages_copied, 1);
    assert_eq!(result.updates_copied, 1);

    // Load the forked session and verify metadata
    let loaded = storage.load_session(&target_info).await.unwrap();

    assert_eq!(loaded.summary.info.id.to_string(), "fork-session-456");
    assert_eq!(loaded.summary.info.cwd, "/new/path");
    assert_eq!(
        loaded.summary.current_model_id,
        acp::ModelId::new("echo-agent-3")
    );
    assert_eq!(
        loaded.summary.parent_session_id,
        Some("source-session-123".to_string())
    );
    assert!(loaded.summary.forked_at.is_some());

    // Verify chat history was copied
    assert_eq!(loaded.chat_history.len(), 1);

    // Verify updates were copied with transformed session ID
    assert_eq!(loaded.updates.len(), 1);
    match &loaded.updates[0] {
        echo_agent_runtime::session::storage::SessionUpdate::Acp(notification) => {
            assert_eq!(notification.session_id.to_string(), "fork-session-456");
        }
        _ => panic!("Expected ACP update"),
    }
}

#[tokio::test]
async fn test_fork_preserves_session_title() {
    let temp_dir = TempDir::new().unwrap();
    let storage = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    // Create source session
    let source_info = create_test_session(&storage, "titled-session", "/source").await;

    // Update source session with a title
    storage
        .update_session_title(&source_info, "My Important Session".to_string())
        .await
        .unwrap();

    // Fork the session
    let target_info = Info {
        id: acp::SessionId::new("fork-titled"),
        cwd: "/new".to_string(),
    };

    let options = echo_agent_runtime::session::storage::CopySessionOptions {
        parent_session_id: Some("titled-session".to_string()),
        new_model_id: None,
        target_prompt_index: None,
        ..Default::default()
    };

    storage
        .copy_session_data(&source_info, &target_info, options)
        .await
        .unwrap();

    // Load and verify title was preserved (generated_title is the LLM title field).
    let loaded = storage.load_session(&target_info).await.unwrap();
    assert_eq!(loaded.summary.display_title(), "My Important Session");
}
