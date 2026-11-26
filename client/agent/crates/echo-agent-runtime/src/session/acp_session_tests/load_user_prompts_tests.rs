use super::*;
use crate::extensions::notification::{
    SessionNotification as EchoAgentNotification, SessionUpdate as EchoAgentSessionUpdate,
};
use crate::session::storage::SessionUpdate;
use agent_client_protocol as acp;

fn write_updates(dir: &std::path::Path, updates: &[SessionUpdate]) -> std::path::PathBuf {
    let path = dir.join("updates.jsonl");
    let mut content = Vec::new();
    for u in updates {
        let envelope = crate::session::storage::SessionUpdateEnvelope::from_update(u).unwrap();
        let mut line = serde_json::to_vec(&envelope).unwrap();
        line.push(b'\n');
        content.extend(line);
    }
    std::fs::write(&path, content).unwrap();
    path
}

fn user_chunk(text: &str) -> SessionUpdate {
    SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        acp::SessionId::new("s1"),
        acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new(text.to_string()),
        ))),
    )))
}

fn agent_chunk(text: &str) -> SessionUpdate {
    SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        acp::SessionId::new("s1"),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new(text.to_string()),
        ))),
    )))
}

fn rewind_marker(target: usize) -> SessionUpdate {
    SessionUpdate::EchoAgent(Box::new(EchoAgentNotification {
        session_id: acp::SessionId::new("s1"),
        update: EchoAgentSessionUpdate::RewindMarker {
            target_prompt_index: target,
            created_at: "2024-01-01T00:00:00Z".to_string(),
        },
        meta: None,
    }))
}

#[test]
fn test_basic_prompt_extraction() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_updates(
        tmp.path(),
        &[
            user_chunk("hello"),
            agent_chunk("hi"),
            user_chunk("fix bug"),
            agent_chunk("done"),
        ],
    );
    let prompts = SessionActor::load_user_prompts_from_updates(&path).unwrap();
    assert_eq!(prompts, vec!["hello", "fix bug"]);
}

#[test]
fn test_rewind_marker_truncates_dead_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_updates(
        tmp.path(),
        &[
            user_chunk("P0"),
            agent_chunk("R0"),
            user_chunk("P1"),
            agent_chunk("R1"),
            user_chunk("P2-old"),
            agent_chunk("R2-old"),
            rewind_marker(1), // rewind to before P1, keeps P0 only
            user_chunk("P1-new"),
            agent_chunk("R1-new"),
        ],
    );
    let prompts = SessionActor::load_user_prompts_from_updates(&path).unwrap();
    // rewind(1) removes P1 and P2-old, next prompt becomes new P1
    assert_eq!(prompts, vec!["P0", "P1-new"]);
}

#[test]
fn test_multiple_rewind_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_updates(
        tmp.path(),
        &[
            user_chunk("P0"),
            agent_chunk("R0"),
            user_chunk("P1"),
            agent_chunk("R1"),
            user_chunk("P2"),
            agent_chunk("R2"),
            rewind_marker(1), // rewind to before P1: keeps P0
            user_chunk("P1v2"),
            agent_chunk("R1v2"),
            rewind_marker(0), // rewind to before P0: keeps nothing
            user_chunk("P0v3"),
            agent_chunk("R0v3"),
        ],
    );
    let prompts = SessionActor::load_user_prompts_from_updates(&path).unwrap();
    // rewind(1) keeps P0, rewind(0) clears all, P0v3 becomes new P0
    assert_eq!(prompts, vec!["P0v3"]);
}

#[test]
fn test_empty_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("updates.jsonl");
    std::fs::write(&path, "").unwrap();
    let prompts = SessionActor::load_user_prompts_from_updates(&path).unwrap();
    assert!(prompts.is_empty());
}

#[test]
fn test_no_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("updates.jsonl");
    let prompts = SessionActor::load_user_prompts_from_updates(&path).unwrap();
    assert!(prompts.is_empty());
}
