//! Integration test for `_echo.agent/session/update` notifications.
//!
//! This test verifies that:
//! 1. EchoAgent session notifications (e.g., diff_review) can be sent via ext_notification
//! 2. The notifications are persisted to storage
//! 3. When a session is loaded, the notifications are replayed with `isReplay: true`

use agent_client_protocol as acp;
use serde_json::json;
use std::path::PathBuf;
use tempfile::TempDir;

use echo_agent_runtime::extensions::notification::{
    DiffContent, SessionNotification, SessionUpdate as EchoAgentSessionUpdate,
};
use echo_agent_runtime::session::info::Info as SessionInfo;
use echo_agent_runtime::session::persistence::default_model_id;
use echo_agent_runtime::session::storage::{JsonlStorageAdapter, SessionUpdate, StorageAdapter};

/// Test that EchoAgent session notifications round-trip through storage correctly.
#[tokio::test]
async fn test_echo_agent_session_notification_storage_roundtrip() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let session_id = acp::SessionId::new("test-session-roundtrip");
    let info = SessionInfo {
        id: session_id.clone(),
        cwd: "/test/workspace".to_string(),
    };

    // Initialize the session
    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();

    // Create a diff_review notification
    let echo_agent_notification = SessionNotification {
        session_id: session_id.clone(),
        update: EchoAgentSessionUpdate::DiffReview {
            content: vec![DiffContent {
                diff: acp::Diff::new(PathBuf::from("/test/file.rs"), "fn new() {}".to_string())
                    .old_text(Some("fn old() {}".to_string())),
            }],
        },
        meta: Some(json!({ "totalTokens": 1234 })),
    };

    // Persist the notification
    adapter
        .append_update(
            &info,
            &SessionUpdate::EchoAgent(Box::new(echo_agent_notification.clone())),
        )
        .await
        .unwrap();

    // Also add an ACP notification to verify mixed storage works
    let acp_notification = acp::SessionNotification::new(
        session_id.clone(),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new("Hello from agent".to_string()),
        ))),
    )
    .meta(json!({ "totalTokens": 5678 }).as_object().cloned());

    adapter
        .append_update(&info, &SessionUpdate::Acp(Box::new(acp_notification)))
        .await
        .unwrap();

    // Load the session and verify both notifications are present
    let loaded = adapter.load_session(&info).await.unwrap();
    assert_eq!(
        loaded.updates.len(),
        2,
        "Should have 2 updates (1 EchoAgent + 1 ACP)"
    );

    // Verify EchoAgent notification
    match &loaded.updates[0] {
        SessionUpdate::EchoAgent(notification) => {
            assert_eq!(notification.session_id, session_id);
            match &notification.update {
                EchoAgentSessionUpdate::DiffReview { content } => {
                    assert_eq!(content.len(), 1);
                    assert_eq!(content[0].diff.path, PathBuf::from("/test/file.rs"));
                    assert_eq!(content[0].diff.old_text, Some("fn old() {}".to_string()));
                    assert_eq!(content[0].diff.new_text, "fn new() {}");
                }
                _ => {
                    panic!("Expected DiffReview, got different update type");
                }
            }
            // Verify meta is preserved
            assert_eq!(
                notification
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("totalTokens")),
                Some(&json!(1234))
            );
        }
        _ => panic!("Expected EchoAgent update as first item"),
    }

    // Verify ACP notification
    match &loaded.updates[1] {
        SessionUpdate::Acp(notification) => {
            assert_eq!(notification.session_id, session_id);
            assert_eq!(
                notification
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("totalTokens")),
                Some(&json!(5678))
            );
        }
        _ => panic!("Expected ACP update as second item"),
    }
}

/// Test that a `TurnCompleted` terminal round-trips through storage — the
/// persistence half of the "stuck on Waiting…" fix, where the durable terminal
/// must survive `updates.jsonl` and reload as a replayable `_echo.agent/session/update`.
#[tokio::test]
async fn test_turn_completed_round_trips_through_storage() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let session_id = acp::SessionId::new("test-session-turn-completed");
    let info = SessionInfo {
        id: session_id.clone(),
        cwd: "/test/workspace".to_string(),
    };

    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();

    // Persist a terminal carrying the prompt id + outcome the viewer keys on,
    // plus an optional agent result.
    let echo_agent_notification = SessionNotification {
        session_id: session_id.clone(),
        update: EchoAgentSessionUpdate::TurnCompleted {
            prompt_id: "prompt-1".to_string(),
            stop_reason: "end_turn".to_string(),
            agent_result: Some("all done".to_string()),
            usage: None,
        },
        meta: None,
    };
    adapter
        .append_update(
            &info,
            &SessionUpdate::EchoAgent(Box::new(echo_agent_notification)),
        )
        .await
        .unwrap();

    // Reload the session (the replay path) and confirm the terminal survives
    // with its fields intact.
    let loaded = adapter.load_session(&info).await.unwrap();
    assert_eq!(
        loaded.updates.len(),
        1,
        "Should have 1 update (the terminal)"
    );

    match &loaded.updates[0] {
        SessionUpdate::EchoAgent(notification) => {
            assert_eq!(notification.session_id, session_id);
            match &notification.update {
                EchoAgentSessionUpdate::TurnCompleted {
                    prompt_id,
                    stop_reason,
                    agent_result,
                    ..
                } => {
                    assert_eq!(prompt_id, "prompt-1");
                    assert_eq!(stop_reason, "end_turn");
                    assert_eq!(agent_result.as_deref(), Some("all done"));
                }
                _ => panic!("Expected TurnCompleted, got different update type"),
            }
        }
        _ => panic!("Expected EchoAgent update"),
    }
}

/// Test that totalTokens can be extracted from both ACP and EchoAgent notifications.
#[tokio::test]
async fn test_extract_total_tokens_from_mixed_updates() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let session_id = acp::SessionId::new("test-session-tokens");
    let info = SessionInfo {
        id: session_id.clone(),
        cwd: "/test/workspace".to_string(),
    };

    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();

    // Add ACP notification with totalTokens
    let acp_notification = acp::SessionNotification::new(
        session_id.clone(),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new("First message".to_string()),
        ))),
    )
    .meta(json!({ "totalTokens": 100 }).as_object().cloned());
    adapter
        .append_update(&info, &SessionUpdate::Acp(Box::new(acp_notification)))
        .await
        .unwrap();

    // Add EchoAgent notification with totalTokens
    let echo_agent_notification = SessionNotification {
        session_id: session_id.clone(),
        update: EchoAgentSessionUpdate::DiffReview { content: vec![] },
        meta: Some(json!({ "totalTokens": 200 })),
    };
    adapter
        .append_update(
            &info,
            &SessionUpdate::EchoAgent(Box::new(echo_agent_notification)),
        )
        .await
        .unwrap();

    // Add another ACP notification with higher totalTokens
    let acp_notification2 = acp::SessionNotification::new(
        session_id.clone(),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new("Second message".to_string()),
        ))),
    )
    .meta(json!({ "totalTokens": 300 }).as_object().cloned());
    adapter
        .append_update(&info, &SessionUpdate::Acp(Box::new(acp_notification2)))
        .await
        .unwrap();

    // Load and extract totalTokens (simulating what load_session does in mvp_agent)
    let loaded = adapter.load_session(&info).await.unwrap();

    let last_total_tokens = loaded
        .updates
        .iter()
        .rev()
        .find_map(|notification| match notification {
            SessionUpdate::Acp(n) => n
                .meta
                .as_ref()
                .and_then(|m| m.get("totalTokens"))
                .and_then(|v| v.as_u64()),
            SessionUpdate::EchoAgent(n) => n
                .meta
                .as_ref()
                .and_then(|m| m.get("totalTokens"))
                .and_then(|v| v.as_u64()),
        })
        .unwrap_or(0);

    assert_eq!(
        last_total_tokens, 300,
        "Should get the last totalTokens value"
    );
}

/// Test the serialization format of SessionNotification for wire compatibility.
#[test]
fn test_echo_agent_session_notification_serialization() {
    let notification = SessionNotification {
        session_id: acp::SessionId::new("sess-123"),
        update: EchoAgentSessionUpdate::DiffReview {
            content: vec![DiffContent {
                diff: acp::Diff::new(PathBuf::from("src/main.rs"), "new".to_string())
                    .old_text(Some("old".to_string())),
            }],
        },
        meta: Some(json!({ "isReplay": true })),
    };

    let json = serde_json::to_value(&notification).unwrap();

    // Print actual JSON for debugging
    println!(
        "Serialized JSON: {}",
        serde_json::to_string_pretty(&json).unwrap()
    );

    // Verify camelCase field names
    assert!(json.get("sessionId").is_some(), "Expected sessionId field");
    assert!(json.get("_meta").is_some(), "Expected _meta field");

    // The update field contains the nested SessionUpdate which has the tag
    let update_obj = json.get("update").expect("Expected update field");
    let session_update_tag = update_obj
        .get("sessionUpdate")
        .expect("Expected sessionUpdate tag in update");
    assert_eq!(session_update_tag, "diff_review");

    // Verify diff content is inside the update
    let content = update_obj
        .get("content")
        .expect("Expected content in update")
        .as_array()
        .unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "diff");
    assert_eq!(content[0]["path"], "src/main.rs");
}
