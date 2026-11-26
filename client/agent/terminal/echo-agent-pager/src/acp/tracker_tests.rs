use super::*;
use std::sync::Arc;
/// Default meta with no timestamps (simulates old echo-agent-shell or tests that
/// don't care about timing).
fn meta() -> NotificationMeta {
    NotificationMeta::default()
}
fn agent_chunk(text: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
        acp::TextContent::new(text.to_string()),
    )))
}
fn thought_chunk(text: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::AgentThoughtChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
        acp::TextContent::new(text.to_string()),
    )))
}
#[test]
fn workflow_suppression_keeps_authoring_calls_visible() {
    let wf = |title: &str, raw: serde_json::Value| {
        acp::ToolCall::new(acp::ToolCallId::new(Arc::from("t1")), title.to_string())
            .kind(acp::ToolKind::Other)
            .status(acp::ToolCallStatus::Pending)
            .raw_input(Some(raw))
    };
    assert!(is_workflow_tool(&wf(
        "Workflow: deep-research",
        serde_json::json!({ "variant": "Workflow", "name": "deep-research" }),
    )));
    assert!(is_workflow_tool(&wf(
        "Workflow: resume run",
        serde_json::json!({ "variant": "Workflow", "resume_from_run_id": "wf_1" }),
    )));
    assert!(!is_workflow_tool(&wf(
        "Validating workflow 'triage'",
        serde_json::json!({ "variant": "Workflow", "script": "let meta = ...", "validate_only": true }),
    )));
    assert!(is_workflow_tool(&wf(
        "Creating workflow 'triage'",
        serde_json::json!({ "variant": "Workflow", "script": "let meta = ..." }),
    )));
    assert!(!is_workflow_tool(&wf(
        "workflow",
        serde_json::json!({ "validate_only": true }),
    )));
    assert!(is_workflow_tool(&wf(
        "workflow",
        serde_json::json!({ "name": "goal" }),
    )));
}
fn tool_call(id: &str, kind: acp::ToolKind, title: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(acp::ToolCallId::new(Arc::from(id)), title.to_string())
            .kind(kind)
            .status(acp::ToolCallStatus::Pending)
            .content(vec![])
            .locations(vec![]),
    )
}
fn tool_call_completed(id: &str, kind: acp::ToolKind, title: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(acp::ToolCallId::new(Arc::from(id)), title.to_string())
            .kind(kind)
            .status(acp::ToolCallStatus::Completed)
            .content(vec![])
            .locations(vec![]),
    )
}
fn tool_update_completed(id: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from(id)),
        acp::ToolCallUpdateFields::new().status(Some(acp::ToolCallStatus::Completed)),
    ))
}
fn user_message(text: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
        acp::TextContent::new(text.to_string()),
    )))
}
#[test]
fn streaming_agent_message() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    assert!(tracker.handle_update(agent_chunk("Hello "), &meta(), &mut sb));
    assert!(tracker.handle_update(agent_chunk("world!"), &meta(), &mut sb));
    assert_eq!(sb.len(), 1);
    assert!(tracker.current_agent_msg.is_some());
}
#[test]
fn agent_output_epoch_tracks_visible_live_output() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    assert!(tracker.handle_update(user_message("prompt"), &meta(), &mut sb));
    assert_eq!(tracker.agent_output_epoch, 0);
    assert!(tracker.handle_update(agent_chunk("response"), &meta(), &mut sb));
    assert_eq!(tracker.agent_output_epoch, 1);
    let replay = NotificationMeta {
        is_replay: true,
        ..Default::default()
    };
    assert!(tracker.handle_update(agent_chunk(" replay"), &replay, &mut sb));
    assert_eq!(tracker.agent_output_epoch, 1);
    assert!(tracker.handle_update(thought_chunk("thinking"), &meta(), &mut sb));
    assert_eq!(tracker.agent_output_epoch, 2);
    assert!(tracker.handle_update(
        tool_call("read-1", acp::ToolKind::Read, "read_file"),
        &meta(),
        &mut sb,
    ));
    assert_eq!(tracker.agent_output_epoch, 3);
    assert!(tracker.handle_update(tool_update_completed("read-1"), &meta(), &mut sb));
    assert_eq!(tracker.agent_output_epoch, 4);
    assert!(!tracker.handle_update(
        tool_call("todo-1", acp::ToolKind::Other, "TodoWrite"),
        &meta(),
        &mut sb,
    ));
    assert_eq!(tracker.agent_output_epoch, 4);
}
#[test]
fn output_since_last_finish_flips_per_turn() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.finish_turn(&mut sb);
    assert!(
        !tracker.output_since_last_finish(),
        "no output right after a finish"
    );
    assert!(tracker.handle_update(agent_chunk("wake reply"), &meta(), &mut sb));
    assert!(
        tracker.output_since_last_finish(),
        "an agent message chunk flips the flag"
    );
    tracker.finish_turn(&mut sb);
    assert!(
        !tracker.output_since_last_finish(),
        "the next finish snapshots the epoch again"
    );
}
#[test]
fn streaming_thinking() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    assert!(tracker.handle_update(thought_chunk("Let me think"), &meta(), &mut sb));
    assert!(tracker.handle_update(thought_chunk("..."), &meta(), &mut sb));
    assert_eq!(sb.len(), 1);
    assert!(tracker.current_thinking.is_some());
}
#[test]
fn pre_create_thinking_no_op_when_flag_off() {
    crate::appearance::cache::set_show_thinking_blocks(false);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.pre_create_thinking(&mut sb);
    assert_eq!(sb.len(), 0);
    assert!(tracker.current_thinking.is_none());
    crate::appearance::cache::set_show_thinking_blocks(true);
}
#[test]
fn thought_chunk_dropped_when_flag_off() {
    crate::appearance::cache::set_show_thinking_blocks(false);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    assert!(!tracker.handle_update(thought_chunk("secret reasoning"), &meta(), &mut sb));
    assert_eq!(sb.len(), 0);
    assert!(tracker.current_thinking.is_none());
    crate::appearance::cache::set_show_thinking_blocks(true);
    assert!(tracker.handle_update(thought_chunk("visible now"), &meta(), &mut sb));
    assert_eq!(sb.len(), 1);
    assert!(tracker.current_thinking.is_some());
}
#[test]
fn pre_create_thinking_creates_when_flag_on() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.pre_create_thinking(&mut sb);
    assert_eq!(sb.len(), 1);
    assert!(tracker.current_thinking.is_some());
}
#[test]
fn thinking_then_agent_message() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("thinking..."), &meta(), &mut sb);
    assert_eq!(sb.len(), 1);
    tracker.handle_update(agent_chunk("Here's my answer"), &meta(), &mut sb);
    assert_eq!(sb.len(), 2);
    assert!(tracker.current_thinking.is_none());
}
#[test]
fn replayed_thinking_uses_server_elapsed_not_local_zero() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let replay_meta = NotificationMeta {
        is_replay: true,
        stream_start_ms: Some(1_000_000),
        agent_timestamp_ms: Some(1_002_000),
        ..NotificationMeta::default()
    };
    tracker.handle_update(thought_chunk("pondering deeply"), &replay_meta, &mut sb);
    tracker.handle_update(agent_chunk("done"), &replay_meta, &mut sb);
    let entries = sb.entries_in_range(0..sb.len());
    let thinking = entries
        .iter()
        .find_map(|e| match &e.block {
            RenderBlock::Thinking(t) => Some(t),
            _ => None,
        })
        .expect("a thinking block should survive replay (non-empty content)");
    assert_eq!(
        thinking.elapsed_time_ms(),
        Some(2000),
        "replayed thinking must use server elapsed, not a ~0ms local-timer freeze"
    );
}
#[test]
fn live_thinking_keeps_local_elapsed_timer() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("thinking live"), &meta(), &mut sb);
    let entries = sb.entries_in_range(0..sb.len());
    let thinking = entries
        .iter()
        .find_map(|e| match &e.block {
            RenderBlock::Thinking(t) => Some(t),
            _ => None,
        })
        .expect("a live thinking block should exist");
    assert!(
        thinking.elapsed_time_ms().is_some(),
        "live thinking must keep a local elapsed timer (started_at armed)"
    );
}
#[test]
fn tool_call_lifecycle() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Read, "src/main.rs"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    assert_eq!(tracker.pending_tools.len(), 1);
    tracker.handle_update(tool_update_completed("tc1"), &meta(), &mut sb);
    assert_eq!(sb.len(), 1);
    assert_eq!(tracker.pending_tools.len(), 0);
}
#[test]
fn tool_call_already_completed() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call_completed("tc1", acp::ToolKind::Read, "src/main.rs"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    assert_eq!(tracker.pending_tools.len(), 0);
}
#[test]
fn agent_msg_resets_after_tool() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(agent_chunk("Before tool"), &meta(), &mut sb);
    assert_eq!(sb.len(), 1);
    tracker.handle_update(
        tool_call_completed("tc1", acp::ToolKind::Read, "file.rs"),
        &meta(),
        &mut sb,
    );
    assert!(tracker.current_agent_msg.is_none());
    tracker.handle_update(agent_chunk("After tool"), &meta(), &mut sb);
    assert_eq!(sb.len(), 3);
}
#[test]
fn finish_turn_clears_state() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(agent_chunk("text"), &meta(), &mut sb);
    tracker.handle_update(thought_chunk("thinking"), &meta(), &mut sb);
    assert!(tracker.current_agent_msg.is_some());
    assert!(tracker.current_thinking.is_some());
    tracker.handle_update(tool_update_completed("tc-orphan"), &meta(), &mut sb);
    assert_eq!(tracker.orphan_updates.len(), 1);
    tracker.task_tool_background.insert("task-x".into(), true);
    tracker.finish_turn(&mut sb);
    assert!(tracker.current_agent_msg.is_none());
    assert!(tracker.current_thinking.is_none());
    assert!(tracker.pending_tools.is_empty());
    assert!(
        tracker.orphan_updates.is_empty(),
        "orphaned tool-call updates are turn-scoped"
    );
    assert_eq!(
        tracker.task_tool_background.get("task-x"),
        Some(&true),
        "background Task flags survive turn end for the late SubagentSpawned"
    );
    assert!(
        !sb.needs_animation(),
        "no entries should be running after finish_turn"
    );
}
#[test]
fn user_message_replay() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(user_message("What is Rust?"), &meta(), &mut sb);
    assert_eq!(sb.len(), 1);
}
#[test]
fn empty_chunks_ignored() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    assert!(!tracker.handle_update(agent_chunk(""), &meta(), &mut sb));
    assert!(!tracker.handle_update(thought_chunk(""), &meta(), &mut sb));
    assert_eq!(sb.len(), 0);
}
/// Regression test: two turns should create separate agent message entries.
///
/// Previously, handle_user_message() didn't reset current_agent_msg,
/// so the second turn's agent message chunks got appended to the first
/// turn's entry, producing concatenated text.
#[test]
fn two_turns_separate_agent_messages() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(user_message("whats the current date"), &meta(), &mut sb);
    tracker.handle_update(thought_chunk("thinking about date..."), &meta(), &mut sb);
    tracker.handle_update(
        agent_chunk("The current date is February 8, 2026."),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(
        user_message("whats the weather in london"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(thought_chunk("thinking about weather..."), &meta(), &mut sb);
    tracker.handle_update(
        agent_chunk("I don't have access to weather data."),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 6, "Each turn should have its own entries");
    let entry2 = sb.get(2).expect("entry 2");
    let entry5 = sb.get(5).expect("entry 5");
    assert_ne!(
        entry2.id, entry5.id,
        "Agent messages from different turns must be separate entries"
    );
}
/// Regression test: user_message should reset tracking state.
#[test]
fn user_message_resets_tracking() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(agent_chunk("hello"), &meta(), &mut sb);
    assert!(tracker.current_agent_msg.is_some());
    tracker.handle_update(user_message("new question"), &meta(), &mut sb);
    assert!(
        tracker.current_agent_msg.is_none(),
        "user_message should reset current_agent_msg"
    );
    assert!(
        tracker.current_thinking.is_none(),
        "user_message should reset current_thinking"
    );
}
/// Regression test: exact real-world flow where send_prompt adds user entry
/// directly to scrollback (bypassing tracker), then tracker receives echo + response.
///
/// This matches what actually happens in the app:
/// 1. send_prompt() pushes user entry + calls expect_user_echo()
/// 2. ACP echoes user_message_chunk → tracker skips it (no duplicate)
/// 3. ACP streams thought_chunk, agent_message_chunk
/// 4. User sends second prompt via send_prompt
/// 5. ACP echoes + streams second turn
///
/// The critical invariant: exactly 1 user entry per turn, 2 separate agent messages.
#[test]
fn real_flow_two_turns_via_send_prompt() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    sb.push_block(RenderBlock::user_prompt("whats the date"));
    tracker.expect_user_echo();
    let modified = tracker.handle_update(user_message("whats the date"), &meta(), &mut sb);
    assert!(!modified, "echo should be skipped, not modify scrollback");
    assert_eq!(sb.len(), 1, "still just 1 entry (direct push only)");
    tracker.handle_update(thought_chunk("thinking about date..."), &meta(), &mut sb);
    tracker.handle_update(
        agent_chunk("Today's date is February 8, 2026."),
        &meta(),
        &mut sb,
    );
    assert!(
        tracker.current_agent_msg.is_some(),
        "turn 1 agent msg should be tracked"
    );
    tracker.finish_turn(&mut sb);
    sb.push_block(RenderBlock::user_prompt("whats the current weather"));
    tracker.expect_user_echo();
    let modified =
        tracker.handle_update(user_message("whats the current weather"), &meta(), &mut sb);
    assert!(!modified, "second echo should also be skipped");
    assert!(
        tracker.current_agent_msg.is_none(),
        "echo should have reset current_agent_msg"
    );
    tracker.handle_update(thought_chunk("thinking about weather..."), &meta(), &mut sb);
    tracker.handle_update(
        agent_chunk("I don't have access to weather data."),
        &meta(),
        &mut sb,
    );
    let agent_msg_indices: Vec<usize> = (0..sb.len())
        .filter(|&i| matches!(sb.get(i).unwrap().block, RenderBlock::AgentMessage(_)))
        .collect();
    assert_eq!(
        agent_msg_indices.len(),
        2,
        "Should have exactly 2 separate agent message entries, got {}. Total entries: {}",
        agent_msg_indices.len(),
        sb.len(),
    );
    let user_count = (0..sb.len())
        .filter(|&i| matches!(sb.get(i).unwrap().block, RenderBlock::UserPrompt(_)))
        .count();
    assert_eq!(
        user_count, 2,
        "exactly 2 user entries (no duplicates from echo)"
    );
}
/// Test: two turns where finish_turn() is called between them
/// (simulating send_prompt calling finish_turn before new turn).
/// No echo user_message_chunk — just direct scrollback manipulation + tracker.
#[test]
fn two_turns_with_finish_turn_between() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    sb.push_block(RenderBlock::user_prompt("whats the date"));
    tracker.handle_update(thought_chunk("thinking..."), &meta(), &mut sb);
    tracker.handle_update(agent_chunk("Today is February 8, 2026."), &meta(), &mut sb);
    assert!(tracker.current_agent_msg.is_some());
    tracker.finish_turn(&mut sb);
    assert!(tracker.current_agent_msg.is_none());
    sb.push_block(RenderBlock::user_prompt("whats the weather"));
    tracker.handle_update(thought_chunk("thinking about weather..."), &meta(), &mut sb);
    tracker.handle_update(agent_chunk("I can't check weather."), &meta(), &mut sb);
    let agent_msg_count = (0..sb.len())
        .filter(|&i| matches!(sb.get(i).unwrap().block, RenderBlock::AgentMessage(_)))
        .count();
    assert_eq!(
        agent_msg_count, 2,
        "Must have 2 separate agent messages, got {}",
        agent_msg_count,
    );
}
/// Test: expect_user_echo skips exactly one echo, then allows normal flow.
#[test]
fn expect_user_echo_skips_one() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    sb.push_block(RenderBlock::user_prompt("hello"));
    tracker.expect_user_echo();
    assert!(!tracker.handle_update(user_message("hello"), &meta(), &mut sb));
    assert_eq!(sb.len(), 1, "echo should not add a duplicate");
    assert!(tracker.handle_update(user_message("world"), &meta(), &mut sb));
    assert_eq!(sb.len(), 2, "second message should be added normally");
}
/// The echoed promptIndex belongs to the turn-starting prompt: an
/// interjection that lands between the local push and the echo (laggy
/// link) must not steal the backfilled index — the shell never numbers
/// interjections.
#[test]
fn echo_prompt_index_backfill_skips_interjections() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let prompt_id = sb.push_block(RenderBlock::user_prompt("real prompt"));
    tracker.expect_user_echo();
    let ij_id = sb.push_block(RenderBlock::interjection_prompt("steer"));
    let echo = acp::SessionUpdate::UserMessageChunk(
        acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
            "real prompt".to_string(),
        )))
        .meta(serde_json::json!({ "promptIndex": 3 }).as_object().cloned()),
    );
    assert!(
        !tracker.handle_update(echo, &meta(), &mut sb),
        "echo is skipped"
    );
    let prompt_idx = sb.index_of_id(prompt_id).unwrap();
    match &sb.get(prompt_idx).unwrap().block {
        RenderBlock::UserPrompt(b) => assert_eq!(b.prompt_index, Some(3)),
        other => panic!("expected UserPrompt, got {other:?}"),
    }
    let ij_idx = sb.index_of_id(ij_id).unwrap();
    match &sb.get(ij_idx).unwrap().block {
        RenderBlock::UserPrompt(b) => {
            assert!(b.is_interjection);
            assert_eq!(
                b.prompt_index, None,
                "interjection must not steal the echoed index"
            );
        }
        other => panic!("expected UserPrompt, got {other:?}"),
    }
}
/// Test: session replay (no expect_user_echo) still creates user entries.
#[test]
fn session_replay_creates_user_entries() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    assert!(tracker.handle_update(user_message("old question"), &meta(), &mut sb));
    assert_eq!(sb.len(), 1);
    tracker.handle_update(agent_chunk("old answer"), &meta(), &mut sb);
    assert!(tracker.handle_update(user_message("second question"), &meta(), &mut sb));
    assert_eq!(sb.len(), 3);
}
/// Skill replay: XML metadata becomes a clean skill block, body is absorbed.
#[test]
fn skill_replay_creates_clean_block() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let xml = "<command-name>implement</command-name>\n\
                <command-message>/implement</command-message>\n\
                <command-args>fix the rendering bug</command-args>";
    assert!(tracker.handle_update(user_message(xml), &meta(), &mut sb));
    assert_eq!(sb.len(), 1);
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::UserPrompt(block) => {
            assert_eq!(
                block.skill_token_ranges,
                vec![0..10],
                "leading /implement token styled as skill"
            );
            assert_eq!(block.text, "/implement fix the rendering bug");
        }
        other => panic!("expected UserPrompt, got {:?}", other),
    }
    assert!(
        !tracker.handle_update(user_message("You are an orchestrator..."), &meta(), &mut sb,),
        "skill body should be absorbed",
    );
    assert_eq!(sb.len(), 1, "no new entry for skill body");
}
/// Skill replay without args still creates a clean block.
#[test]
fn skill_replay_no_args() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let xml = "<command-name>deploy</command-name>\n\
                <command-message>/deploy</command-message>";
    assert!(tracker.handle_update(user_message(xml), &meta(), &mut sb));
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::UserPrompt(block) => {
            assert_eq!(block.skill_token_ranges, vec![0..7]);
            assert_eq!(block.text, "/deploy");
        }
        other => panic!("expected UserPrompt, got {:?}", other),
    }
    assert!(!tracker.handle_update(user_message("Deploy instructions"), &meta(), &mut sb,));
    assert_eq!(sb.len(), 1);
}
/// Live execution: echo-skip + skill body skip work together.
#[test]
fn skill_echo_skips_both_chunks() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    sb.push_block(RenderBlock::skill_prompt("/implement fix bug"));
    tracker.expect_user_echo();
    let xml = "<command-name>implement</command-name>\n\
                <command-message>/implement</command-message>\n\
                <command-args>fix bug</command-args>";
    assert!(!tracker.handle_update(user_message(xml), &meta(), &mut sb));
    assert_eq!(sb.len(), 1, "echo should not add a duplicate");
    assert!(!tracker.handle_update(user_message("You are an orchestrator..."), &meta(), &mut sb,));
    assert_eq!(sb.len(), 1, "skill body echo should be absorbed");
    assert!(tracker.handle_update(user_message("follow-up question"), &meta(), &mut sb,));
    assert_eq!(sb.len(), 2);
}
/// finish_turn clears stale skip_next_skill_body.
#[test]
fn finish_turn_clears_skill_body_skip() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let xml = "<command-name>commit</command-name>\n\
                <command-message>/commit</command-message>";
    tracker.handle_update(user_message(xml), &meta(), &mut sb);
    assert!(tracker.skip_next_skill_body);
    tracker.finish_turn(&mut sb);
    assert!(!tracker.skip_next_skill_body);
    assert!(tracker.handle_update(user_message("new question"), &meta(), &mut sb,));
    assert_eq!(sb.len(), 2);
}
#[test]
fn tool_update_before_tool_call_race() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    assert!(!tracker.handle_update(tool_update_completed("tc1"), &meta(), &mut sb));
    assert_eq!(sb.len(), 0);
    assert_eq!(tracker.orphan_updates.len(), 1);
    assert!(tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `echo hi`"),
        &meta(),
        &mut sb,
    ));
    assert_eq!(sb.len(), 1);
    assert_eq!(tracker.orphan_updates.len(), 0);
    assert_eq!(tracker.pending_tools.len(), 0);
}
#[test]
fn tool_update_before_tool_call_preserves_kind() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(tool_update_completed("tc1"), &meta(), &mut sb);
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `ls`"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(_)) => {}
        other => panic!("Expected Execute block, got {:?}", other),
    }
}
#[test]
fn tool_normal_order_still_works() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Read, "src/lib.rs"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    assert_eq!(tracker.pending_tools.len(), 1);
    assert_eq!(tracker.orphan_updates.len(), 0);
    tracker.handle_update(tool_update_completed("tc1"), &meta(), &mut sb);
    assert_eq!(sb.len(), 1);
    assert_eq!(tracker.pending_tools.len(), 0);
}
/// Test thinking elapsed time computed from server timestamps.
#[test]
fn thinking_elapsed_from_server_timestamps() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let stream_start = 1700000000000i64;
    let make_meta = |agent_ts: i64| NotificationMeta {
        agent_timestamp_ms: Some(agent_ts),
        stream_start_ms: Some(stream_start),
        ..Default::default()
    };
    tracker.handle_update(
        thought_chunk("Let me think"),
        &make_meta(stream_start + 500),
        &mut sb,
    );
    assert_eq!(tracker.last_thinking_elapsed_ms, Some(500));
    tracker.handle_update(
        thought_chunk("...still thinking"),
        &make_meta(stream_start + 3200),
        &mut sb,
    );
    assert_eq!(tracker.last_thinking_elapsed_ms, Some(3200));
    tracker.handle_update(agent_chunk("Here's my answer"), &meta(), &mut sb);
    assert!(tracker.current_thinking.is_none());
    assert_eq!(tracker.last_thinking_elapsed_ms, None);
}
/// Test thinking elapsed is None when server doesn't send timestamps.
#[test]
fn thinking_elapsed_none_without_timestamps() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("thinking"), &meta(), &mut sb);
    assert_eq!(tracker.last_thinking_elapsed_ms, None);
    tracker.handle_update(agent_chunk("done"), &meta(), &mut sb);
    assert_eq!(tracker.last_thinking_elapsed_ms, None);
}
#[test]
fn agent_message_uses_server_timestamp() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let ts_ms = 1700000000000i64;
    let replay_meta = NotificationMeta {
        agent_timestamp_ms: Some(ts_ms),
        is_replay: true,
        ..Default::default()
    };
    tracker.handle_update(agent_chunk("Hello"), &replay_meta, &mut sb);
    let entry = sb.get(0).unwrap();
    let created = entry.created_at.expect("entry should have created_at");
    let expected = utc_ms_to_local(ts_ms);
    assert_eq!(
        created.timestamp(),
        expected.timestamp(),
        "Agent message should use server timestamp, not Local::now()"
    );
}
#[test]
fn user_message_uses_server_timestamp() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let ts_ms = 1700000000000i64;
    let replay_meta = NotificationMeta {
        turn_start_ms: Some(ts_ms),
        is_replay: true,
        ..Default::default()
    };
    tracker.handle_update(user_message("Hello user"), &replay_meta, &mut sb);
    let entry = sb.get(0).unwrap();
    let created = entry.created_at.expect("entry should have created_at");
    let expected = utc_ms_to_local(ts_ms);
    assert_eq!(
        created.timestamp(),
        expected.timestamp(),
        "User message should use server turn_start_ms timestamp"
    );
}
#[test]
fn entry_falls_back_to_now_without_server_timestamp() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let before = chrono::Local::now();
    tracker.handle_update(agent_chunk("live message"), &meta(), &mut sb);
    let after = chrono::Local::now();
    let entry = sb.get(0).unwrap();
    let created = entry.created_at.expect("entry should have created_at");
    assert!(
        created >= before && created <= after,
        "Without server timestamp, should fall back to Local::now()"
    );
}
/// Helper: create a ToolCallUpdate with InProgress status and BashOutput raw_output.
fn tool_update_in_progress(id: &str, output_bytes: &[u8]) -> acp::SessionUpdate {
    use echo_agent_tools::types::output::{BashOutput, ToolOutput};
    let bash = BashOutput {
        output: output_bytes.to_vec(),
        output_for_prompt: String::new(),
        exit_code: 0,
        command: String::new(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: None,
        current_dir: String::new(),
        output_file: String::new(),
        total_bytes: output_bytes.len(),
        output_delta: None,
        was_bare_echo: false,
    };
    acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from(id)),
        acp::ToolCallUpdateFields::new()
            .status(Some(acp::ToolCallStatus::InProgress))
            .raw_output(serde_json::to_value(ToolOutput::Bash(bash)).ok()),
    ))
}
/// Helper: create a completed ToolCallUpdate with BashOutput.
fn tool_update_completed_bash(id: &str, output_bytes: &[u8], exit_code: i32) -> acp::SessionUpdate {
    use echo_agent_tools::types::output::{BashOutput, ToolOutput};
    let status = if exit_code == 0 {
        acp::ToolCallStatus::Completed
    } else {
        acp::ToolCallStatus::Failed
    };
    let bash = BashOutput {
        output: output_bytes.to_vec(),
        output_for_prompt: String::new(),
        exit_code,
        command: "test".to_string(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: None,
        current_dir: "/tmp".to_string(),
        output_file: String::new(),
        total_bytes: output_bytes.len(),
        output_delta: None,
        was_bare_echo: false,
    };
    acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from(id)),
        acp::ToolCallUpdateFields::new()
            .status(Some(status))
            .raw_output(serde_json::to_value(ToolOutput::Bash(bash)).ok()),
    ))
}
/// Helper: create a ToolCall with raw_input containing command + description.
fn tool_call_execute_with_desc(id: &str, command: &str, description: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from(id)),
            format!("Execute `{}`", command),
        )
        .kind(acp::ToolKind::Execute)
        .status(acp::ToolCallStatus::Completed)
        .content(vec![])
        .raw_input(Some(serde_json::json!({
            "command": command,
            "description": description,
        })))
        .locations(vec![]),
    )
}
/// Streaming execute: InProgress updates push output to the block.
#[test]
fn streaming_execute_in_progress() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `cargo build`"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    assert_eq!(tracker.pending_tools.len(), 1);
    let modified = tracker.handle_update(
        tool_update_in_progress("tc1", b"Compiling"),
        &meta(),
        &mut sb,
    );
    assert!(modified, "InProgress update should modify scrollback");
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) => {
            assert_eq!(exec.output.as_deref(), Some("Compiling"));
        }
        other => panic!("Expected Execute block, got {:?}", other),
    }
    tracker.handle_update(
        tool_update_in_progress("tc1", b"Compiling crate v0.1.0\n  Finished"),
        &meta(),
        &mut sb,
    );
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) => {
            assert_eq!(
                exec.output.as_deref(),
                Some("Compiling crate v0.1.0\n  Finished")
            );
        }
        other => panic!("Expected Execute block, got {:?}", other),
    }
    assert_eq!(tracker.pending_tools.len(), 1);
    assert!(sb.get(0).unwrap().is_running);
}
/// Streaming execute: completed update replaces block with final output.
#[test]
fn streaming_execute_completion() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `echo hello`"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(tool_update_in_progress("tc1", b"hello\n"), &meta(), &mut sb);
    tracker.handle_update(
        tool_update_completed_bash("tc1", b"hello\n", 0),
        &meta(),
        &mut sb,
    );
    assert_eq!(tracker.pending_tools.len(), 0);
    assert!(!sb.get(0).unwrap().is_running);
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) => {
            assert_eq!(exec.output.as_deref(), Some("hello\n"));
            assert!(exec.error.is_none(), "exit code 0 = no error");
        }
        other => panic!("Expected Execute block, got {:?}", other),
    }
}
/// Streaming execute: failed command shows error.
#[test]
fn streaming_execute_failure() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `false`"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(tool_update_completed_bash("tc1", b"", 1), &meta(), &mut sb);
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) => {
            assert!(exec.error.is_some(), "non-zero exit should set error");
            assert!(
                exec.error.as_deref().unwrap().contains("exit code 1"),
                "error should mention exit code"
            );
        }
        other => panic!("Expected Execute block, got {:?}", other),
    }
}
/// Execute with description from raw_input.
#[test]
fn execute_with_description() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call_execute_with_desc("tc1", "cargo test", "Run the test suite"),
        &meta(),
        &mut sb,
    );
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) => {
            assert_eq!(exec.command, "cargo test");
            assert_eq!(exec.description.as_deref(), Some("Run the test suite"));
        }
        other => panic!("Expected Execute block, got {:?}", other),
    }
}
/// InProgress update for non-execute tool is ignored.
#[test]
fn in_progress_update_ignored_for_non_execute() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Read, "src/main.rs"),
        &meta(),
        &mut sb,
    );
    let modified = tracker.handle_update(
        tool_update_in_progress("tc1", b"file content"),
        &meta(),
        &mut sb,
    );
    assert!(
        !modified,
        "InProgress with bash output should be ignored for Read blocks"
    );
}
/// Output is passed through without modification (no-color mode means
/// the shell sends clean output without ANSI codes).
#[test]
fn streaming_execute_passes_output_through() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `ls`"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(
        tool_update_in_progress("tc1", b"green text"),
        &meta(),
        &mut sb,
    );
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) => {
            assert_eq!(
                exec.output.as_deref(),
                Some("green text"),
                "Output should be passed through as-is"
            );
        }
        other => panic!("Expected Execute block, got {:?}", other),
    }
}
/// Verify ToolOutput::Bash round-trips through serde_json::Value correctly.
/// This mimics the exact path: streaming_local_terminal serializes with
/// serde_json::to_value(ToolOutput::Bash(...)), and tracker deserializes with
/// serde_json::from_value::<ToolOutput>(...).
#[test]
fn tool_output_bash_serde_roundtrip() {
    use echo_agent_tools::types::output::{BashOutput, ToolOutput};
    let bash = BashOutput {
        output: b"hello world\n".to_vec(),
        output_for_prompt: String::new(),
        exit_code: 0,
        command: "echo hello".to_string(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: None,
        current_dir: "/tmp".to_string(),
        output_file: String::new(),
        total_bytes: 12,
        output_delta: None,
        was_bare_echo: false,
    };
    let value = serde_json::to_value(ToolOutput::Bash(bash)).unwrap();
    assert_eq!(
        value.get("type").and_then(|v| v.as_str()),
        Some("Bash"),
        "ToolOutput should serialize with type tag"
    );
    assert!(value.get("output").is_some(), "Should have output field");
    let deserialized: ToolOutput = serde_json::from_value(value).unwrap();
    match deserialized {
        ToolOutput::Bash(bash) => {
            assert_eq!(bash.output, b"hello world\n");
            assert_eq!(bash.command, "echo hello");
        }
        _ => panic!("Expected ToolOutput::Bash"),
    }
}
/// End-to-end test mimicking the exact production notification sequence:
/// 1. ToolCall (Pending) with raw_input containing BashTool
/// 2. InProgress ToolCallUpdate with raw_output containing ToolOutput::Bash
///    (sent by notification_bridge from LocalTerminalBackend)
/// 3. Completed ToolCallUpdate with raw_output containing final ToolOutput::Bash
/// 4. Second Completed ToolCallUpdate (from acp_session completion handler)
#[test]
fn production_execute_sequence() {
    use echo_agent_tools::types::output::{BashOutput, ToolOutput};
    use serde_json::json;
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let tc_id = "call_abc123";
    let tc = acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from(tc_id)),
            "Execute `python tmp/test.py`".to_string(),
        )
        .kind(acp::ToolKind::Execute)
        .status(acp::ToolCallStatus::Pending)
        .content(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
            acp::TextContent::new("Running Python script".to_string()),
        ))])
        .raw_input(Some(json!({
            "command": "python tmp/test.py",
            "description": "Running Python script"
        })))
        .locations(vec![]),
    );
    tracker.handle_update(tc, &meta(), &mut sb);
    assert_eq!(sb.len(), 1);
    assert_eq!(tracker.pending_tools.len(), 1);
    let bash_output = ToolOutput::Bash(BashOutput {
        output_for_prompt: String::new(),
        output: b"Step 1: loading...\n".to_vec(),
        exit_code: 0,
        command: "python tmp/test.py".to_string(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: None,
        current_dir: "/tmp".to_string(),
        output_file: String::new(),
        total_bytes: 19,
        output_delta: None,
        was_bare_echo: false,
    });
    let in_progress = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from(tc_id)),
        acp::ToolCallUpdateFields::new()
            .status(Some(acp::ToolCallStatus::InProgress))
            .content(Some(vec![acp::ToolCallContent::from(
                acp::ContentBlock::Text(acp::TextContent::new("Step 1: loading...\n".to_string())),
            )]))
            .raw_output(serde_json::to_value(&bash_output).ok()),
    ));
    let modified = tracker.handle_update(in_progress, &meta(), &mut sb);
    assert!(modified, "InProgress should trigger redraw");
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) => {
            assert_eq!(
                exec.output.as_deref(),
                Some("Step 1: loading...\n"),
                "Streaming output should be set"
            );
        }
        other => panic!("Expected Execute block, got {:?}", other),
    }
    let bash_output2 = ToolOutput::Bash(BashOutput {
        output_for_prompt: String::new(),
        output: b"Step 1: loading...\nStep 2: processing...\n".to_vec(),
        exit_code: 0,
        command: "python tmp/test.py".to_string(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: None,
        current_dir: "/tmp".to_string(),
        output_file: String::new(),
        total_bytes: 40,
        output_delta: None,
        was_bare_echo: false,
    });
    let in_progress2 = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from(tc_id)),
        acp::ToolCallUpdateFields::new()
            .status(Some(acp::ToolCallStatus::InProgress))
            .raw_output(serde_json::to_value(&bash_output2).ok()),
    ));
    tracker.handle_update(in_progress2, &meta(), &mut sb);
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) => {
            assert_eq!(
                exec.output.as_deref(),
                Some("Step 1: loading...\nStep 2: processing...\n"),
                "Output should be replaced with full buffer"
            );
        }
        other => panic!("Expected Execute block, got {:?}", other),
    }
    let final_bash = ToolOutput::Bash(BashOutput {
        output_for_prompt: String::new(),
        output: b"Step 1: loading...\nStep 2: processing...\nDone!\n".to_vec(),
        exit_code: 0,
        command: "python tmp/test.py".to_string(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: None,
        current_dir: "/tmp".to_string(),
        output_file: String::new(),
        total_bytes: 46,
        output_delta: None,
        was_bare_echo: false,
    });
    let completed = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from(tc_id)),
        acp::ToolCallUpdateFields::new()
            .status(Some(acp::ToolCallStatus::Completed))
            .raw_output(serde_json::to_value(&final_bash).ok()),
    ));
    tracker.handle_update(completed, &meta(), &mut sb);
    assert_eq!(tracker.pending_tools.len(), 0);
    let entry = sb.get(0).unwrap();
    assert!(!entry.is_running, "Should be marked as not running");
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) => {
            assert!(exec.output.is_some(), "Final block should have output");
            let output = exec.output.as_deref().unwrap();
            assert!(
                output.contains("Done!"),
                "Should contain final output, got: {output}"
            );
        }
        other => panic!("Expected Execute block, got {:?}", other),
    }
}
#[test]
fn utf8_decoder_ascii_passthrough() {
    let mut dec = Utf8Decoder::default();
    assert_eq!(dec.decode(b"hello"), "hello");
    assert!(dec.buffer.is_empty());
}
#[test]
fn utf8_decoder_complete_multibyte() {
    let mut dec = Utf8Decoder::default();
    assert_eq!(dec.decode("café".as_bytes()), "café");
    assert!(dec.buffer.is_empty());
}
#[test]
fn utf8_decoder_split_2byte_char() {
    let mut dec = Utf8Decoder::default();
    assert_eq!(dec.decode(&[b'c', b'a', b'f', 0xC3]), "caf");
    assert_eq!(dec.buffer, &[0xC3]);
    assert_eq!(dec.decode(&[0xA9]), "é");
    assert!(dec.buffer.is_empty());
}
#[test]
fn utf8_decoder_split_3byte_char() {
    let mut dec = Utf8Decoder::default();
    assert_eq!(dec.decode(&[0xE2]), "");
    assert_eq!(dec.buffer, &[0xE2]);
    assert_eq!(dec.decode(&[0x9C]), "");
    assert_eq!(dec.buffer, &[0xE2, 0x9C]);
    assert_eq!(dec.decode(&[0x93, b'!']), "✓!");
    assert!(dec.buffer.is_empty());
}
#[test]
fn utf8_decoder_split_4byte_char() {
    let mut dec = Utf8Decoder::default();
    assert_eq!(dec.decode(&[0xF0, 0x9F]), "");
    assert_eq!(dec.buffer, &[0xF0, 0x9F]);
    assert_eq!(dec.decode(&[0xA6, 0x80]), "🦀");
    assert!(dec.buffer.is_empty());
}
#[test]
fn utf8_decoder_genuinely_invalid_byte() {
    let mut dec = Utf8Decoder::default();
    let result = dec.decode(&[b'a', 0xFF, b'b']);
    assert_eq!(result, "a\u{FFFD}b");
    assert!(dec.buffer.is_empty());
}
#[test]
fn utf8_decoder_multiple_feeds() {
    let mut dec = Utf8Decoder::default();
    assert_eq!(dec.decode(b"line1\n"), "line1\n");
    assert_eq!(dec.decode(b"line2\n"), "line2\n");
    assert_eq!(dec.decode("héllo\n".as_bytes()), "héllo\n");
    assert!(dec.buffer.is_empty());
}
/// Reproduce the exact ACP message flow for a grep search tool call:
/// 1. ToolCall with kind=Other, title="grep" (initial, no metadata)
/// 2. ToolCallUpdate in-progress with kind=search, title="fn main", rawInput
/// 3. ToolCallUpdate completed with rawOutput containing GrepSearchOutput
///
/// This was broken: kind from in-progress update was lost, so the completed
/// block rendered as "Other" with no search results.
#[test]
fn test_search_tool_call_flow() {
    use echo_agent_tools::types::output::{GrepFileMatch, GrepLineMatch, GrepSearchOutput};
    let mut tracker = AcpUpdateTracker::new();
    let mut scrollback = ScrollbackState::new();
    let tc_id: Arc<str> = Arc::from("toolu_search_001");
    let tool_call = acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(acp::ToolCallId::new(tc_id.clone()), "grep".to_string())
            .kind(acp::ToolKind::Other)
            .status(acp::ToolCallStatus::Pending),
    );
    tracker.handle_update(tool_call, &meta(), &mut scrollback);
    assert_eq!(scrollback.len(), 1);
    let in_progress = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(tc_id.clone()),
        acp::ToolCallUpdateFields::new()
            .kind(Some(acp::ToolKind::Search))
            .title(Some("fn main".to_string()))
            .raw_input(Some(serde_json::json!({
                "variant": "Grep",
                "pattern": "fn main",
                "path": "src/",
            }))),
    ));
    tracker.handle_update(in_progress, &meta(), &mut scrollback);
    assert_eq!(scrollback.len(), 1, "should still be 1 entry");
    let entry = scrollback.get(0).expect("entry exists");
    assert!(
        matches!(
            &entry.block,
            RenderBlock::ToolCall(ToolCallBlock::Search(_))
        ),
        "block should be Search after in-progress update, got: {:?}",
        std::mem::discriminant(&entry.block)
    );
    let grep_output = GrepSearchOutput {
        stdout: vec![],
        stderr: vec![],
        exit_code: 0,
        match_count: 1,
        file_matches: vec![GrepFileMatch {
            path: "/Users/alice/dev/rust/foo/src/main.rs".to_string(),
            matches: vec![GrepLineMatch {
                line_number: 54,
                content: "fn main() -> Result<()> {".to_string(),
            }],
        }],
    };
    let completed = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(tc_id.clone()),
        acp::ToolCallUpdateFields::new()
            .status(Some(acp::ToolCallStatus::Completed))
            .content(Some(vec![acp::ToolCallContent::Content(
                acp::Content::new(acp::ContentBlock::Text(acp::TextContent::new(
                    "found 1 matches".to_string(),
                ))),
            )]))
            .raw_output(serde_json::to_value(ToolOutput::GrepSearch(grep_output)).ok()),
    ));
    tracker.handle_update(completed, &meta(), &mut scrollback);
    assert_eq!(scrollback.len(), 1, "should still be 1 entry");
    let entry = scrollback.get(0).expect("entry exists");
    if let RenderBlock::ToolCall(ToolCallBlock::Search(search)) = &entry.block {
        assert_eq!(search.pattern, "fn main");
        assert_eq!(search.match_count, 1);
        assert_eq!(search.file_matches.len(), 1);
        assert_eq!(
            search.file_matches[0].path,
            "/Users/alice/dev/rust/foo/src/main.rs"
        );
        assert_eq!(search.file_matches[0].matches.len(), 1);
        assert_eq!(search.file_matches[0].matches[0].line_number, 54);
        assert_eq!(
            search.file_matches[0].matches[0].content,
            "fn main() -> Result<()> {"
        );
    } else {
        panic!(
            "Expected Search block after completion, got: {:?}",
            std::mem::discriminant(&entry.block)
        );
    }
}
/// ScrollbackState with an explicit `expanded_by_default` shape override
/// (flag-independent: the `Some` beats the `collapsed_edit_blocks` cache).
fn edit_config_scrollback(expanded_by_default: bool) -> ScrollbackState {
    use crate::appearance::AppearanceConfig;
    let mut sb = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.blocks.edit.expanded_by_default = Some(expanded_by_default);
    sb.set_appearance(appearance);
    sb
}
/// ToolCall(Pending) with kind=Other (shell currently sends this).
fn pending_other_tool_call(tc_id: &Arc<str>) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(
            acp::ToolCallId::new(tc_id.clone()),
            "search_replace".to_string(),
        )
        .kind(acp::ToolKind::Other)
        .status(acp::ToolCallStatus::Pending)
        .content(vec![])
        .locations(vec![]),
    )
}
/// Regression test for #199720 follow-up: when an Other(Pending) entry is
/// upgraded in-place to an Edit block, the entry's `display_mode` must be
/// reset to the materialize policy's default — Collapsed by default,
/// Expanded when `expanded_by_default` is set — rather than left at
/// Other's default.
///
/// Also covers the fast-path Pending→Completed (no in-progress refinement)
/// where Edit's `finished_display_mode()` returns `None` and `finish_running`
/// would otherwise leave a stale mode in place.
#[test]
fn edit_tool_upgrade_resets_display_mode_to_default() {
    use crate::scrollback::types::DisplayMode;
    /// Drive Pending(Other) → InProgress(Edit) → Completed and return the
    /// display mode observed after the InProgress upgrade and after
    /// completion.
    fn upgrade_path(tc: &str, expanded_by_default: bool) -> (DisplayMode, DisplayMode) {
        let mut tracker = AcpUpdateTracker::new();
        let mut sb = edit_config_scrollback(expanded_by_default);
        let tc_id: Arc<str> = Arc::from(tc);
        tracker.handle_update(pending_other_tool_call(&tc_id), &meta(), &mut sb);
        assert_eq!(sb.len(), 1);
        assert_eq!(sb.get(0).unwrap().display_mode, DisplayMode::Collapsed);
        let in_progress = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            acp::ToolCallId::new(tc_id.clone()),
            acp::ToolCallUpdateFields::new()
                .kind(Some(acp::ToolKind::Edit))
                .title(Some("foo.rs".to_string()))
                .raw_input(Some(serde_json::json!({ "file_path": "foo.rs" }))),
        ));
        tracker.handle_update(in_progress, &meta(), &mut sb);
        let entry = sb.get(0).expect("entry exists");
        assert!(
            matches!(&entry.block, RenderBlock::ToolCall(ToolCallBlock::Edit(_))),
            "block should be upgraded to Edit after in-progress refinement"
        );
        let after_upgrade = entry.display_mode;
        tracker.handle_update(tool_update_completed(&tc_id), &meta(), &mut sb);
        (after_upgrade, sb.get(0).unwrap().display_mode)
    }
    /// Drive the fast path Pending(Other) → Completed(Edit) with no
    /// in-progress refinement and return the final display mode.
    fn fast_path(tc: &str, expanded_by_default: bool) -> DisplayMode {
        let mut tracker = AcpUpdateTracker::new();
        let mut sb = edit_config_scrollback(expanded_by_default);
        let tc_id: Arc<str> = Arc::from(tc);
        tracker.handle_update(pending_other_tool_call(&tc_id), &meta(), &mut sb);
        assert_eq!(sb.get(0).unwrap().display_mode, DisplayMode::Collapsed);
        let completed = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            acp::ToolCallId::new(tc_id.clone()),
            acp::ToolCallUpdateFields::new()
                .kind(Some(acp::ToolKind::Edit))
                .title(Some("foo.rs".to_string()))
                .raw_input(Some(serde_json::json!({ "file_path": "foo.rs" })))
                .status(Some(acp::ToolCallStatus::Completed)),
        ));
        tracker.handle_update(completed, &meta(), &mut sb);
        let entry = sb.get(0).expect("entry exists");
        assert!(
            matches!(&entry.block, RenderBlock::ToolCall(ToolCallBlock::Edit(_))),
            "block should be Edit after fast Pending→Completed"
        );
        entry.display_mode
    }
    let (upgraded, completed) = upgrade_path("toolu_edit_001", false);
    assert_eq!(
        upgraded,
        DisplayMode::Collapsed,
        "collapse shape: Edit upgrade stays Collapsed"
    );
    assert_eq!(
        completed,
        DisplayMode::Collapsed,
        "collapse shape: successful Edit remains Collapsed after completion"
    );
    assert_eq!(
        fast_path("toolu_edit_002", false),
        DisplayMode::Collapsed,
        "collapse shape: fast Pending→Completed Edit ends up Collapsed"
    );
    let (upgraded, completed) = upgrade_path("toolu_edit_003", true);
    assert_eq!(
        upgraded,
        DisplayMode::Expanded,
        "config on: display_mode must be reset to Expanded on upgrade, \
         not left at Other's default (Collapsed)"
    );
    assert_eq!(
        completed,
        DisplayMode::Expanded,
        "config on: successful Edit remains Expanded after completion"
    );
    assert_eq!(
        fast_path("toolu_edit_004", true),
        DisplayMode::Expanded,
        "config on: fast Pending→Completed Edit must end up Expanded"
    );
}
/// A manual expand of the collapsed one-liner must survive completion:
/// once the entry is an Edit, the Edit-to-Edit completion swap preserves
/// the current mode instead of snapping back to the configured default
/// (no `respect_manual_folds` pinning required).
#[test]
fn edit_manual_expand_survives_completion() {
    use crate::scrollback::types::DisplayMode;
    let mut tracker = AcpUpdateTracker::new();
    let mut sb = edit_config_scrollback(false);
    let tc_id: Arc<str> = Arc::from("toolu_edit_gesture");
    tracker.handle_update(pending_other_tool_call(&tc_id), &meta(), &mut sb);
    let in_progress = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(tc_id.clone()),
        acp::ToolCallUpdateFields::new()
            .kind(Some(acp::ToolKind::Edit))
            .title(Some("foo.rs".to_string()))
            .raw_input(Some(serde_json::json!({ "file_path": "foo.rs" })))
            .content(Some(vec![acp::ToolCallContent::Diff(
                acp::Diff::new("foo.rs", "let x = 2;\n".to_string())
                    .old_text(Some("let x = 1;\n".to_string())),
            )])),
    ));
    tracker.handle_update(in_progress, &meta(), &mut sb);
    assert_eq!(sb.get(0).unwrap().display_mode, DisplayMode::Collapsed);
    sb.get_by_id_mut(sb.get(0).unwrap().id)
        .unwrap()
        .set_display_mode(DisplayMode::Expanded);
    tracker.handle_update(tool_update_completed(&tc_id), &meta(), &mut sb);
    assert_eq!(
        sb.get(0).unwrap().display_mode,
        DisplayMode::Expanded,
        "completion must not snap a user-expanded Edit back to Collapsed"
    );
}
/// Multi-file (apply_patch shape: several Diff items) and title-fallback
/// Edits can't be summarized by the one-liner: they materialize Expanded
/// with the summary marked untrusted, config-independent. Each case
/// isolates one untrusted signal.
#[test]
fn multi_diff_and_title_fallback_edits_default_expanded() {
    use crate::scrollback::types::DisplayMode;
    let diff = |path: &str, old: &str, new: &str| {
        acp::ToolCallContent::Diff(
            acp::Diff::new(path, new.to_string()).old_text(Some(old.to_string())),
        )
    };
    let assert_untrusted_expanded = |tc: acp::ToolCall, label: &str| {
        let mut tracker = AcpUpdateTracker::new();
        let mut sb = edit_config_scrollback(false);
        tracker.handle_update(acp::SessionUpdate::ToolCall(tc), &meta(), &mut sb);
        let entry = sb.get(0).expect("entry exists");
        let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &entry.block else {
            panic!("{label}: expected Edit block, got {:?}", entry.block);
        };
        assert!(edit.summary_untrusted, "{label}: summary must be untrusted");
        assert_eq!(
            entry.display_mode,
            DisplayMode::Expanded,
            "{label}: untrusted summaries must not collapse to the one-liner"
        );
    };
    assert_untrusted_expanded(
        acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from("toolu_multi_diff")),
            "Apply patch".to_string(),
        )
        .kind(acp::ToolKind::Edit)
        .status(acp::ToolCallStatus::Completed)
        .raw_input(Some(serde_json::json!({ "file_path": "a.rs" })))
        .content(vec![
            diff("a.rs", "a1\n", "a2\n"),
            diff("b.rs", "b1\n", "b2\n"),
        ])
        .locations(vec![]),
        "multi_diff",
    );
    assert_untrusted_expanded(
        acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from("toolu_title_fallback")),
            "Apply patch".to_string(),
        )
        .kind(acp::ToolKind::Edit)
        .status(acp::ToolCallStatus::Completed)
        .content(vec![diff("a.rs", "a1\n", "a2\n")])
        .locations(vec![]),
        "title_fallback",
    );
}
/// ToolCall(Pending) start for a search_replace edit.
fn edit_tool_start(id: &str) -> acp::SessionUpdate {
    tool_call(id, acp::ToolKind::Edit, "search_replace")
}
/// Diff content replacing one line at `line`, so each scripted edit
/// yields exactly one `+1/-1` hunk at a distinct position.
fn edit_diff_content(path: &str, line: usize) -> acp::ToolCallContent {
    acp::ToolCallContent::Diff(
        acp::Diff::new(path, format!("new_{line}"))
            .old_text(Some(format!("old_{line}")))
            .meta(
                serde_json::json!({ "old_line": line, "new_line": line })
                    .as_object()
                    .cloned(),
            ),
    )
}
/// Completed update carrying the edit's file_path and one-hunk diff.
fn edit_tool_complete(id: &str, path: &str, line: usize) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from(id)),
        acp::ToolCallUpdateFields::new()
            .kind(Some(acp::ToolKind::Edit))
            .title(Some(path.to_string()))
            .raw_input(Some(serde_json::json!({ "file_path": path })))
            .content(Some(vec![edit_diff_content(path, line)]))
            .status(Some(acp::ToolCallStatus::Completed)),
    ))
}
/// Full Pending → Completed lifecycle for one scripted edit.
fn run_edit(
    tracker: &mut AcpUpdateTracker,
    sb: &mut ScrollbackState,
    id: &str,
    path: &str,
    line: usize,
) {
    tracker.handle_update(edit_tool_start(id), &meta(), sb);
    tracker.handle_update(edit_tool_complete(id, path, line), &meta(), sb);
}
/// Pre-completed ToolCall (replay / session-load shape) with the same
/// one-hunk diff as [`edit_tool_complete`].
fn edit_tool_precompleted(id: &str, path: &str, line: usize) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(acp::ToolCallId::new(Arc::from(id)), path.to_string())
            .kind(acp::ToolKind::Edit)
            .status(acp::ToolCallStatus::Completed)
            .raw_input(Some(serde_json::json!({ "file_path": path })))
            .content(vec![edit_diff_content(path, line)])
            .locations(vec![]),
    )
}
fn edit_block_at(sb: &ScrollbackState, idx: usize) -> &EditToolCallBlock {
    match &sb.get(idx).expect("entry at index").block {
        RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) => edit,
        other => panic!("expected Edit block at {idx}, got {other:?}"),
    }
}
/// Positions of the edited lines, one per hunk, in hunk order.
fn hunk_lines(edit: &EditToolCallBlock) -> Vec<usize> {
    edit.hunks
        .iter()
        .map(|h| {
            h.iter()
                .find(|l| l.tag == similar::ChangeTag::Insert)
                .expect("insert line")
                .ln
        })
        .collect()
}
#[test]
fn adjacent_same_file_edits_coalesce() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        run_edit(&mut tracker, &mut sb, "e1", "foo.rs", 5);
        run_edit(&mut tracker, &mut sb, "e2", "foo.rs", 40);
        assert_eq!(sb.len(), 1, "two adjacent edits must merge into one entry");
        let edit = edit_block_at(&sb, 0);
        assert_eq!(edit.hunks.len(), 2);
        assert_eq!(edit.edit_count, 2);
        assert_eq!(hunk_lines(edit), vec![5, 40], "hunks keep scrollback order");
        let inserts: usize = edit
            .hunks
            .iter()
            .flatten()
            .filter(|l| l.tag == similar::ChangeTag::Insert)
            .count();
        assert_eq!(inserts, 2);
    })
    .join()
    .unwrap();
}
#[test]
fn overlapping_adjacent_edits_stitch_into_single_hunk() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        for (i, line) in (5..=9).enumerate() {
            run_edit(&mut tracker, &mut sb, &format!("e{i}"), "foo.rs", line);
        }
        assert_eq!(sb.len(), 1);
        let edit = edit_block_at(&sb, 0);
        assert_eq!(edit.hunks.len(), 1, "contiguous hunks stitch into one");
        assert_eq!(
            edit.edit_count, 5,
            "the (N edits) fallback counts merged calls, not stitched hunks"
        );
        let rows: Vec<(similar::ChangeTag, usize)> =
            edit.hunks[0].iter().map(|l| (l.tag, l.ln)).collect();
        let expected: Vec<(similar::ChangeTag, usize)> = (5..=9)
            .flat_map(|ln| {
                [
                    (similar::ChangeTag::Delete, ln),
                    (similar::ChangeTag::Insert, ln),
                ]
            })
            .collect();
        assert_eq!(rows, expected);
    })
    .join()
    .unwrap();
}
#[test]
fn coalesce_disabled_when_collapsed_edit_blocks_off() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(false);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        run_edit(&mut tracker, &mut sb, "e1", "foo.rs", 5);
        run_edit(&mut tracker, &mut sb, "e2", "foo.rs", 40);
        assert_eq!(
            sb.len(),
            2,
            "flag off keeps the legacy one-row-per-call transcript"
        );
        assert_eq!(edit_block_at(&sb, 0).hunks.len(), 1);
        assert_eq!(edit_block_at(&sb, 1).hunks.len(), 1);
    })
    .join()
    .unwrap();
}
#[test]
fn three_sequential_edits_chain_into_one() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        run_edit(&mut tracker, &mut sb, "e1", "foo.rs", 5);
        run_edit(&mut tracker, &mut sb, "e2", "foo.rs", 20);
        run_edit(&mut tracker, &mut sb, "e3", "foo.rs", 40);
        assert_eq!(sb.len(), 1);
        let edit = edit_block_at(&sb, 0);
        assert_eq!(edit.hunks.len(), 3);
        assert_eq!(hunk_lines(edit), vec![5, 20, 40]);
    })
    .join()
    .unwrap();
}
#[test]
fn different_files_do_not_coalesce() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        run_edit(&mut tracker, &mut sb, "e1", "foo.rs", 5);
        run_edit(&mut tracker, &mut sb, "e2", "bar.rs", 5);
        assert_eq!(sb.len(), 2, "edits to different files stay separate");
    })
    .join()
    .unwrap();
}
#[test]
fn intervening_entry_breaks_coalesce_run() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        run_edit(&mut tracker, &mut sb, "e1", "foo.rs", 5);
        tracker.handle_update(agent_chunk("first edit done"), &meta(), &mut sb);
        run_edit(&mut tracker, &mut sb, "e2", "foo.rs", 40);
        assert_eq!(
            sb.len(),
            3,
            "a visible entry between edits blocks the merge"
        );
        assert_eq!(edit_block_at(&sb, 0).hunks.len(), 1);
        assert_eq!(edit_block_at(&sb, 2).hunks.len(), 1);
    })
    .join()
    .unwrap();
}
#[test]
fn parallel_out_of_order_completion_coalesces() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        tracker.handle_update(edit_tool_start("e1"), &meta(), &mut sb);
        tracker.handle_update(edit_tool_start("e2"), &meta(), &mut sb);
        tracker.handle_update(edit_tool_complete("e2", "foo.rs", 40), &meta(), &mut sb);
        assert_eq!(sb.len(), 2, "no merge while the earlier call still runs");
        tracker.handle_update(edit_tool_complete("e1", "foo.rs", 5), &meta(), &mut sb);
        assert_eq!(sb.len(), 1, "forward check merges once the earlier lands");
        let edit = edit_block_at(&sb, 0);
        assert_eq!(
            hunk_lines(edit),
            vec![5, 40],
            "push order, not completion order"
        );
    })
    .join()
    .unwrap();
}
#[test]
fn errored_edit_does_not_coalesce() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        run_edit(&mut tracker, &mut sb, "e1", "foo.rs", 5);
        tracker.handle_update(edit_tool_start("e2"), &meta(), &mut sb);
        tracker.handle_update(
            acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from("e2")),
                acp::ToolCallUpdateFields::new()
                    .kind(Some(acp::ToolKind::Edit))
                    .raw_input(Some(serde_json::json!({ "file_path": "foo.rs" })))
                    .status(Some(acp::ToolCallStatus::Failed)),
            )),
            &meta(),
            &mut sb,
        );
        assert_eq!(sb.len(), 2, "a failed edit never merges");
        assert!(edit_block_at(&sb, 1).error.is_some());
    })
    .join()
    .unwrap();
}
#[test]
fn committed_edit_does_not_coalesce() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        run_edit(&mut tracker, &mut sb, "e1", "foo.rs", 5);
        sb.mark_committed(0);
        run_edit(&mut tracker, &mut sb, "e2", "foo.rs", 40);
        assert_eq!(sb.len(), 2, "a committed row never merges");
        assert_eq!(edit_block_at(&sb, 0).hunks.len(), 1);
        assert_eq!(edit_block_at(&sb, 1).hunks.len(), 1);
    })
    .join()
    .unwrap();
}
#[test]
fn untrusted_summary_edit_does_not_coalesce() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        run_edit(&mut tracker, &mut sb, "e1", "foo.rs", 5);
        let multi_diff =
            acp::ToolCall::new(acp::ToolCallId::new(Arc::from("e2")), "foo.rs".to_string())
                .kind(acp::ToolKind::Edit)
                .status(acp::ToolCallStatus::Completed)
                .raw_input(Some(serde_json::json!({ "file_path": "foo.rs" })))
                .content(vec![
                    edit_diff_content("foo.rs", 40),
                    edit_diff_content("bar.rs", 7),
                ])
                .locations(vec![]);
        tracker.handle_update(acp::SessionUpdate::ToolCall(multi_diff), &meta(), &mut sb);
        assert_eq!(sb.len(), 2, "an untrusted summary never merges");
        assert!(edit_block_at(&sb, 1).summary_untrusted);
    })
    .join()
    .unwrap();
}
#[test]
fn replay_precompleted_edits_coalesce_without_hl_queue() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        let replay = NotificationMeta {
            is_replay: true,
            ..Default::default()
        };
        tracker.handle_update(edit_tool_precompleted("e1", "foo.rs", 5), &replay, &mut sb);
        tracker.handle_update(edit_tool_precompleted("e2", "foo.rs", 40), &replay, &mut sb);
        assert_eq!(sb.len(), 1, "replayed adjacent edits merge like live ones");
        assert_eq!(hunk_lines(edit_block_at(&sb, 0)), vec![5, 40]);
        assert!(
            tracker.take_pending_edit_hl().is_empty(),
            "replay never queues full-file HL"
        );
    })
    .join()
    .unwrap();
}
#[test]
fn coalesce_repoints_pending_edit_hl_to_survivor() {
    std::thread::spawn(|| {
        crate::appearance::cache::set_collapsed_edit_blocks(true);
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        run_edit(&mut tracker, &mut sb, "e1", "foo.rs", 5);
        run_edit(&mut tracker, &mut sb, "e2", "foo.rs", 40);
        let survivor = sb.get(0).unwrap().id;
        assert_eq!(
            tracker.take_pending_edit_hl(),
            vec![survivor],
            "HL queue holds the survivor exactly once, never the removed id"
        );
    })
    .join()
    .unwrap();
}
fn scrollback_with_respect_manual_folds() -> ScrollbackState {
    use crate::appearance::AppearanceConfig;
    let mut sb = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.scroll.respect_manual_folds = true;
    sb.set_appearance(appearance);
    sb
}
#[test]
fn pinned_thinking_keeps_user_mode_across_finish_triggers() {
    use crate::scrollback::types::DisplayMode;
    crate::appearance::cache::set_show_thinking_blocks(true);
    let setup = || {
        let mut sb = scrollback_with_respect_manual_folds();
        let mut tracker = AcpUpdateTracker::new();
        tracker.handle_update(thought_chunk("deep thought"), &meta(), &mut sb);
        sb.prepare_layout(80, 40);
        sb.set_selected(Some(0));
        sb.expand_selected();
        let entry = sb.get(0).unwrap();
        assert!(entry.display_mode_pinned, "manual expand pins the entry");
        assert_eq!(entry.display_mode, DisplayMode::Expanded);
        (tracker, sb)
    };
    let assert_kept = |sb: &ScrollbackState, trigger: &str| {
        let entry = sb.get(0).unwrap();
        assert!(!entry.is_running, "{trigger}: thinking finished");
        assert_eq!(
            entry.display_mode,
            DisplayMode::Expanded,
            "{trigger}: pinned thinking must keep the user's mode"
        );
    };
    let (mut tracker, mut sb) = setup();
    tracker.handle_update(agent_chunk("answer"), &meta(), &mut sb);
    assert_kept(&sb, "agent chunk");
    let (mut tracker, mut sb) = setup();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Read, "src/main.rs"),
        &meta(),
        &mut sb,
    );
    assert_kept(&sb, "tool call");
    let (mut tracker, mut sb) = setup();
    tracker.finish_turn(&mut sb);
    assert_kept(&sb, "finish_turn");
    let mut sb = scrollback_with_respect_manual_folds();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("deep thought"), &meta_stream(1000), &mut sb);
    sb.prepare_layout(80, 40);
    sb.set_selected(Some(0));
    sb.expand_selected();
    tracker.handle_update(thought_chunk("new stream"), &meta_stream(2000), &mut sb);
    assert_kept(&sb, "stream restart");
    let mut sb = scrollback_with_respect_manual_folds();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("deep thought"), &meta(), &mut sb);
    tracker.handle_update(agent_chunk("answer"), &meta(), &mut sb);
    let entry = sb.get(0).unwrap();
    assert!(!entry.is_running);
    assert_eq!(
        entry.display_mode,
        DisplayMode::Collapsed,
        "unpinned thinking still auto-collapses"
    );
}
#[test]
fn pinned_execute_keeps_user_mode_across_block_upgrades() {
    use crate::scrollback::types::DisplayMode;
    let mut sb = scrollback_with_respect_manual_folds();
    let mut tracker = AcpUpdateTracker::new();
    let tc_id = "call_pinned_exec";
    tracker.handle_update(
        tool_call(tc_id, acp::ToolKind::Execute, "Execute `sleep 5`"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(
        tool_update_in_progress(tc_id, b"tick 1\n"),
        &meta(),
        &mut sb,
    );
    sb.prepare_layout(80, 40);
    sb.set_selected(Some(0));
    sb.expand_selected();
    let entry = sb.get(0).unwrap();
    assert!(
        entry.display_mode_pinned,
        "expand after the first output tick (which makes Execute foldable) pins the entry"
    );
    assert_eq!(entry.display_mode, DisplayMode::Expanded);
    tracker.handle_update(
        tool_update_in_progress(tc_id, b"tick 1\ntick 2\n"),
        &meta(),
        &mut sb,
    );
    let entry = sb.get(0).unwrap();
    assert_eq!(
        entry.display_mode,
        DisplayMode::Expanded,
        "InProgress block upgrade must not reset a pinned entry (agent Execute default is Collapsed)"
    );
    assert!(entry.display_mode_pinned, "pin survives the block swap");
    tracker.handle_update(tool_update_completed(tc_id), &meta(), &mut sb);
    let entry = sb.get(0).unwrap();
    assert!(!entry.is_running);
    assert_eq!(
        entry.display_mode,
        DisplayMode::Expanded,
        "Completed upgrade + finish must not reset a pinned entry"
    );
}
#[test]
fn respect_manual_folds_off_bypasses_finish_pin_guard() {
    use crate::appearance::AppearanceConfig;
    use crate::scrollback::types::DisplayMode;
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = scrollback_with_respect_manual_folds();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("deep thought"), &meta(), &mut sb);
    sb.prepare_layout(80, 40);
    sb.set_selected(Some(0));
    sb.expand_selected();
    sb.collapse_selected();
    let entry = sb.get(0).unwrap();
    assert!(entry.display_mode_pinned);
    assert_eq!(entry.display_mode, DisplayMode::Truncated);
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.scroll.respect_manual_folds = false;
    sb.set_appearance(appearance);
    tracker.handle_update(agent_chunk("answer"), &meta(), &mut sb);
    assert_eq!(
        sb.get(0).unwrap().display_mode,
        DisplayMode::Collapsed,
        "flag off: finish applies the sticky mode to a pinned non-Expanded entry"
    );
}
/// Helper: build a NotificationMeta with a specific stream_start_ms.
fn meta_stream(stream_start: i64) -> NotificationMeta {
    NotificationMeta {
        stream_start_ms: Some(stream_start),
        agent_timestamp_ms: Some(stream_start + 100),
        ..Default::default()
    }
}
/// Regression test: agent message (stream A) → thinking (stream B) → agent message (stream B).
///
/// Without stream_start_ms boundary detection, stream B's agent message
/// chunks were appended to stream A's entry because handle_thought_chunk
/// never resets current_agent_msg.
#[test]
fn stream_start_breaks_agent_msg_across_streams() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let stream_a = meta_stream(1000);
    let stream_b = meta_stream(2000);
    tracker.handle_update(thought_chunk("thinking A"), &stream_a, &mut sb);
    tracker.handle_update(agent_chunk("answer A"), &stream_a, &mut sb);
    assert_eq!(sb.len(), 2);
    tracker.handle_update(thought_chunk("thinking B"), &stream_b, &mut sb);
    tracker.handle_update(agent_chunk("answer B"), &stream_b, &mut sb);
    assert_eq!(sb.len(), 4, "Each stream should produce separate entries");
    let agent_indices: Vec<usize> = (0..sb.len())
        .filter(|&i| matches!(sb.get(i).unwrap().block, RenderBlock::AgentMessage(_)))
        .collect();
    assert_eq!(
        agent_indices.len(),
        2,
        "Should have 2 separate agent message entries"
    );
    assert_ne!(
        sb.get(agent_indices[0]).unwrap().id,
        sb.get(agent_indices[1]).unwrap().id,
    );
}
/// Same stream_start_ms should NOT break messages — chunks append normally.
#[test]
fn same_stream_start_appends_normally() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let stream = meta_stream(1000);
    tracker.handle_update(agent_chunk("Hello "), &stream, &mut sb);
    tracker.handle_update(agent_chunk("world!"), &stream, &mut sb);
    assert_eq!(sb.len(), 1, "Same stream should append to one entry");
}
/// stream_start_ms change breaks thinking entries too.
#[test]
fn stream_start_breaks_thinking_across_streams() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let stream_a = meta_stream(1000);
    let stream_b = meta_stream(2000);
    tracker.handle_update(thought_chunk("thinking A"), &stream_a, &mut sb);
    assert!(tracker.current_thinking.is_some());
    tracker.handle_update(thought_chunk("thinking B"), &stream_b, &mut sb);
    assert_eq!(sb.len(), 2, "Each stream should get its own thinking entry");
    assert!(
        !sb.get(0).unwrap().is_running,
        "stream A thinking should be finished"
    );
}
/// Agent message in stream A, then agent message in stream B (no thinking between).
#[test]
fn stream_start_breaks_agent_msg_to_agent_msg() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let stream_a = meta_stream(1000);
    let stream_b = meta_stream(2000);
    tracker.handle_update(agent_chunk("message A"), &stream_a, &mut sb);
    tracker.handle_update(agent_chunk("message B"), &stream_b, &mut sb);
    assert_eq!(
        sb.len(),
        2,
        "Different streams should create separate agent messages"
    );
    assert!(
        !sb.get(0).unwrap().is_running,
        "stream A message should be finished"
    );
}
/// No stream_start_ms (old echo-agent-shell) should not break anything.
#[test]
fn no_stream_start_ms_preserves_existing_behavior() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(agent_chunk("Hello "), &meta(), &mut sb);
    tracker.handle_update(agent_chunk("world!"), &meta(), &mut sb);
    assert_eq!(
        sb.len(),
        1,
        "Without stream_start_ms, chunks should append normally"
    );
}
/// finish_turn resets last_stream_start_ms so the next turn starts fresh.
#[test]
fn finish_turn_resets_stream_start() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let stream = meta_stream(1000);
    tracker.handle_update(agent_chunk("turn 1"), &stream, &mut sb);
    assert_eq!(tracker.last_stream_start_ms, Some(1000));
    tracker.finish_turn(&mut sb);
    assert_eq!(
        tracker.last_stream_start_ms, None,
        "finish_turn should reset last_stream_start_ms"
    );
    tracker.handle_update(agent_chunk("turn 2"), &stream, &mut sb);
    assert_eq!(sb.len(), 2);
}
#[test]
fn activity_none_by_default() {
    let tracker = AcpUpdateTracker::new();
    assert_eq!(tracker.activity(), None);
}
#[test]
fn activity_thinking_when_thought_chunks_arrive() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("hmm..."), &meta(), &mut sb);
    assert_eq!(tracker.activity(), Some(TurnActivity::Thinking));
}
#[test]
fn activity_responding_when_agent_chunks_arrive() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(agent_chunk("Here's my answer"), &meta(), &mut sb);
    assert_eq!(tracker.activity(), Some(TurnActivity::Responding));
}
#[test]
fn activity_thinking_to_responding_transition() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("thinking..."), &meta(), &mut sb);
    assert_eq!(tracker.activity(), Some(TurnActivity::Thinking));
    tracker.handle_update(agent_chunk("answer"), &meta(), &mut sb);
    assert_eq!(tracker.activity(), Some(TurnActivity::Responding));
}
#[test]
fn activity_tool_running_when_tool_pending() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "cargo test"),
        &meta(),
        &mut sb,
    );
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::ToolRunning {
            title: "cargo test".into(),
            description: None,
        })
    );
}
/// Foreground execute tools often carry a human `description` in raw_input
/// (e.g. sleep with "Wait 5 seconds…"). Surface it for the spinner.
#[test]
fn activity_tool_running_prefers_description_from_raw_input() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(
                acp::ToolCallId::new(Arc::from("tc1")),
                "run_terminal_command",
            )
            .kind(acp::ToolKind::Execute)
            .status(acp::ToolCallStatus::Pending)
            .content(vec![])
            .raw_input(Some(serde_json::json!({
                "command": "sleep 5 && echo done",
                "description": "Wait 5 seconds then print done",
            })))
            .locations(vec![]),
        ),
        &meta(),
        &mut sb,
    );
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::ToolRunning {
            title: "sleep 5 && echo done".into(),
            description: Some("Wait 5 seconds then print done".into()),
        })
    );
}
/// The initial ToolCall registers with kind=Other and title=tool_id
/// (e.g. "Shell"). When raw_input carries a `command` field, activity()
/// should show the command instead of the bare tool name.
#[test]
fn activity_extracts_command_from_raw_input_regardless_of_kind() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let tc = acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(acp::ToolCallId::new(Arc::from("tc1")), "Shell".to_string())
            .kind(acp::ToolKind::Other)
            .status(acp::ToolCallStatus::Pending)
            .content(vec![])
            .raw_input(Some(
                serde_json::json!({ "command": "gt stack submit --no-edit" }),
            ))
            .locations(vec![]),
    );
    tracker.handle_update(tc, &meta(), &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::ToolRunning {
            title: "gt stack submit --no-edit".into(),
            description: None
        }),
    );
}
#[test]
fn activity_strips_redundant_session_cd_prefix() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let cwd = std::path::PathBuf::from("/proj");
    tracker.set_session_cwd(cwd.clone());
    let command = format!("cd {} && echo hi", cwd.display());
    let tc = acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from("tc-cd")),
            "Shell".to_string(),
        )
        .kind(acp::ToolKind::Execute)
        .status(acp::ToolCallStatus::Pending)
        .content(vec![])
        .raw_input(Some(serde_json::json!({ "command": command })))
        .locations(vec![]),
    );
    tracker.handle_update(tc, &meta(), &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::ToolRunning {
            title: "echo hi".into(),
            description: None
        }),
    );
}
#[test]
fn activity_strips_windows_shaped_session_cd_on_unix_host() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let cwd = std::path::PathBuf::from(r"C:\Users\a\proj");
    tracker.set_session_cwd(cwd.clone());
    let command = r"cd C:\Users\a\proj && cargo test";
    let tc = acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from("tc-win")),
            "Shell".to_string(),
        )
        .kind(acp::ToolKind::Execute)
        .status(acp::ToolCallStatus::Pending)
        .content(vec![])
        .raw_input(Some(serde_json::json!({ "command": command })))
        .locations(vec![]),
    );
    tracker.handle_update(tc, &meta(), &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::ToolRunning {
            title: "cargo test".into(),
            description: None
        }),
    );
}
#[test]
fn execute_block_keeps_full_command_sets_header_display_when_peeled() {
    let tc = acp::ToolCall::new(acp::ToolCallId::new(Arc::from("tc-exec")), "Execute")
        .kind(acp::ToolKind::Execute)
        .status(acp::ToolCallStatus::Completed)
        .content(vec![])
        .raw_input(Some(
            serde_json::json!({ "command": "cd /proj && echo hi" }),
        ))
        .locations(vec![]);
    let block = tool_call_to_block(&tc, Some(Path::new("/proj")));
    match &block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) => {
            assert_eq!(exec.command, "cd /proj && echo hi");
            assert_eq!(exec.header_display.as_deref(), Some("echo hi"));
        }
        other => panic!("expected Execute block, got {other:?}"),
    }
    assert_eq!(block.copy_meta().as_deref(), Some("cd /proj && echo hi"));
    let searchable = block.searchable_text().expect("searchable");
    assert!(
        searchable.contains("cd /proj && echo hi"),
        "searchable_text must retain full command: {searchable}"
    );
}
#[test]
fn activity_none_after_finish_turn() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(agent_chunk("text"), &meta(), &mut sb);
    assert_eq!(tracker.activity(), Some(TurnActivity::Responding));
    tracker.finish_turn(&mut sb);
    assert_eq!(tracker.activity(), None);
}
#[test]
fn activity_compaction_overrides_other_state() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(agent_chunk("text"), &meta(), &mut sb);
    assert_eq!(tracker.activity(), Some(TurnActivity::Responding));
    tracker.set_compaction_activity(Some(TurnActivity::AutoCompacting));
    assert_eq!(tracker.activity(), Some(TurnActivity::AutoCompacting));
    tracker.finish_turn(&mut sb);
    assert_eq!(tracker.activity(), None);
}
/// Nameless start → named upgrade; only visible label changes redraw.
#[test]
fn activity_writing_tool_call_labels_and_redraws() {
    let mut tracker = AcpUpdateTracker::new();
    assert!(tracker.note_tool_call_arguments_delta(None, 0));
    let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
        panic!("expected WritingToolCall activity");
    };
    assert_eq!(writing.label(), "Preparing tool call…");
    assert!(tracker.note_tool_call_arguments_delta(Some("write"), 0));
    let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
        panic!("expected WritingToolCall activity");
    };
    assert_eq!(writing.label(), "Writing file…");
    assert!(!tracker.note_tool_call_arguments_delta(None, 0));
    assert!(!tracker.note_tool_call_arguments_delta(Some("write"), 0));
}
/// First-party tools with long argument streams read as friendly phrases
/// (wire spellings pinned per toolset); tiny-payload read-style tools keep
/// the raw-name fallback.
#[test]
fn activity_writing_tool_call_labels_first_party_writing_tools() {
    for (name, expected) in [
        ("write", "Writing file…"),
        ("search_replace", "Writing edit…"),
        ("edit", "Writing edit…"),
        ("hashline_edit", "Writing edit…"),
        ("apply_patch", "Writing edit…"),
        ("run_terminal_command", "Writing command…"),
        ("run_terminal_cmd", "Writing command…"),
        ("bash", "Writing command…"),
        ("todo_write", "Updating todo list…"),
        ("todowrite", "Updating todo list…"),
        ("workflow", "Writing workflow…"),
        ("image_gen", "Writing image prompt…"),
        ("image_edit", "Writing image prompt…"),
        ("image_to_video", "Writing video prompt…"),
        ("reference_to_video", "Writing video prompt…"),
        ("ask_user_question", "Preparing question…"),
        ("read_file", "Preparing read_file…"),
    ] {
        let mut tracker = AcpUpdateTracker::new();
        tracker.note_tool_call_arguments_delta(Some(name), 0);
        let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
            panic!("expected WritingToolCall activity for {name:?}");
        };
        assert_eq!(writing.label(), expected, "label for {name:?}");
    }
    let mut tracker = AcpUpdateTracker::new();
    tracker.note_tool_call_arguments_delta(Some("search_replace"), 0);
    tracker.note_tool_call_arguments_delta(Some("search_replace"), 1);
    let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
        panic!("expected WritingToolCall activity");
    };
    assert_eq!(writing.label(), "Writing edit (2)…");
}
/// Every taxonomy-mapped spelling must have copy here: a spelling whose kind
/// misses the copy match would silently keep the raw-name fallback.
#[test]
fn activity_writing_tool_call_copy_covers_taxonomy_map() {
    for (name, _) in echo_agent_tools::tool_taxonomy::WRITING_TOOL_WIRE_NAMES {
        let mut tracker = AcpUpdateTracker::new();
        tracker.note_tool_call_arguments_delta(Some(name), 0);
        let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
            panic!("expected WritingToolCall activity for {name:?}");
        };
        assert_ne!(
            writing.label(),
            format!("Preparing {name}…"),
            "mapped spelling {name:?} fell back to the raw name"
        );
    }
}
/// A silent delta stream expires from the spinner but stays visible to
/// lost-response recovery as a dead-stream signal; a new delta re-reveals.
#[test]
fn activity_writing_tool_call_expires_when_deltas_go_stale() {
    let mut tracker = AcpUpdateTracker::new();
    tracker.note_tool_call_arguments_delta(Some("write"), 0);
    assert!(matches!(
        tracker.activity(),
        Some(TurnActivity::WritingToolCall(_))
    ));
    assert!(!tracker.has_stale_tool_call_write());
    tracker.backdate_last_tool_call_delta(
        WRITING_DELTA_STALE_AFTER + std::time::Duration::from_secs(1),
    );
    assert_eq!(tracker.activity(), None);
    assert!(tracker.has_stale_tool_call_write());
    assert!(tracker.note_tool_call_arguments_delta(None, 0));
    assert!(matches!(
        tracker.activity(),
        Some(TurnActivity::WritingToolCall(_))
    ));
}
#[test]
fn activity_writing_tool_call_prettifies_qualified_mcp_names() {
    let mut tracker = AcpUpdateTracker::new();
    tracker.note_tool_call_arguments_delta(Some("linear__list_issues"), 0);
    let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
        panic!("expected WritingToolCall activity");
    };
    assert_eq!(writing.label(), "Preparing (Linear) List Issues…");
}
/// The MCP dispatch/discovery wire names read as friendly phrases, not raw ids.
#[test]
fn activity_writing_tool_call_names_mcp_dispatch_tools() {
    let mut tracker = AcpUpdateTracker::new();
    tracker.note_tool_call_arguments_delta(Some("use_tool"), 0);
    let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
        panic!("expected WritingToolCall activity");
    };
    assert_eq!(writing.label(), "Preparing MCP tool…");
    tracker.note_tool_call_arguments_delta(Some("use_tool"), 1);
    let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
        panic!("expected WritingToolCall activity");
    };
    assert_eq!(writing.label(), "Preparing MCP tool (2)…");
    let mut tracker = AcpUpdateTracker::new();
    tracker.note_tool_call_arguments_delta(Some("search_tool"), 0);
    let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
        panic!("expected WritingToolCall activity");
    };
    assert_eq!(writing.label(), "Searching MCP tools…");
}
#[test]
fn activity_writing_subagent_prompt_for_task_tools() {
    for name in ["task", "Task", "spawn_subagent"] {
        let mut tracker = AcpUpdateTracker::new();
        tracker.note_tool_call_arguments_delta(Some(name), 0);
        let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
            panic!("expected WritingToolCall activity for {name:?}");
        };
        assert_eq!(writing.label(), "Writing subagent prompt…");
    }
}
#[test]
fn activity_writing_tool_call_overrides_open_thinking() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("planning the edit…"), &meta(), &mut sb);
    assert_eq!(tracker.activity(), Some(TurnActivity::Thinking));
    tracker.note_tool_call_arguments_delta(Some("search_replace"), 0);
    assert!(matches!(
        tracker.activity(),
        Some(TurnActivity::WritingToolCall(_))
    ));
    assert!(
        tracker.current_thinking.is_some(),
        "thinking scrollback block must stay open until the canonical ToolCall"
    );
}
#[test]
fn writing_tool_call_cleared_by_canonical_tool_call() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.note_tool_call_arguments_delta(Some("read_file"), 0);
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Read, "read_file"),
        &meta(),
        &mut sb,
    );
    assert!(
        matches!(tracker.activity(), Some(TurnActivity::ToolRunning { .. })),
        "canonical ToolCall must replace the writing label"
    );
    let mut tracker = AcpUpdateTracker::new();
    tracker.note_tool_call_arguments_delta(Some("task"), 0);
    tracker.handle_update(
        tool_call("t2", acp::ToolKind::Other, "task"),
        &meta(),
        &mut sb,
    );
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::subagent())),
        "suppressed foreground task tool must show its Subagent wait, not a stale writing label"
    );
}
#[test]
fn writing_tool_call_cleared_on_new_stream_start() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("round 1"), &meta_stream(1_000), &mut sb);
    tracker.note_tool_call_arguments_delta(Some("write"), 0);
    assert!(matches!(
        tracker.activity(),
        Some(TurnActivity::WritingToolCall(_))
    ));
    tracker.handle_update(thought_chunk("round 2"), &meta_stream(2_000), &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Thinking),
        "a new stream's thought must clear the stale writing label"
    );
}
/// Retrying outranks WritingToolCall, so a delta must clear the stale override.
#[test]
fn writing_tool_call_delta_clears_retry_activity() {
    let retrying = TurnActivity::Retrying {
        attempt: 2,
        max_retries: 5,
        reason: "overloaded".into(),
    };
    let mut tracker = AcpUpdateTracker::new();
    tracker.set_retry_activity(Some(retrying.clone()));
    assert!(tracker.note_tool_call_arguments_delta(Some("write"), 0));
    assert!(matches!(
        tracker.activity(),
        Some(TurnActivity::WritingToolCall(_))
    ));
    tracker.set_retry_activity(Some(retrying));
    assert!(matches!(
        tracker.activity(),
        Some(TurnActivity::Retrying { .. })
    ));
    assert!(
        tracker.note_tool_call_arguments_delta(None, 0),
        "clearing the retry mask must request a redraw"
    );
    assert!(matches!(
        tracker.activity(),
        Some(TurnActivity::WritingToolCall(_))
    ));
    assert!(
        !tracker.note_tool_call_arguments_delta(None, 0),
        "steady-state continuation deltas still need no redraw"
    );
}
#[test]
fn writing_tool_call_ordinal_counts_parallel_calls() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let label = |tracker: &AcpUpdateTracker| {
        let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
            panic!("expected WritingToolCall activity");
        };
        writing.label()
    };
    assert!(tracker.note_tool_call_arguments_delta(Some("spawn_subagent"), 0));
    assert_eq!(label(&tracker), "Writing subagent prompt…");
    assert!(tracker.note_tool_call_arguments_delta(Some("spawn_subagent"), 1));
    assert_eq!(label(&tracker), "Writing subagent prompt (2)…");
    assert!(!tracker.note_tool_call_arguments_delta(None, 1));
    assert!(tracker.note_tool_call_arguments_delta(Some("read_file"), 2));
    assert_eq!(label(&tracker), "Preparing read_file (3)…");
    assert!(tracker.note_tool_call_arguments_delta(None, 3));
    assert_eq!(label(&tracker), "Preparing tool call (4)…");
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Read, "read_file"),
        &meta(),
        &mut sb,
    );
    assert!(tracker.note_tool_call_arguments_delta(Some("write"), 0));
    assert_eq!(label(&tracker), "Writing file…");
}
#[test]
fn waiting_payload_and_writing_churn_are_not_phase_transitions() {
    let wait = |subject: Option<&str>| {
        Some(TurnActivity::Waiting(WaitingReason::Subagent {
            display: subject.map(str::to_string),
        }))
    };
    assert!(!is_phase_transition(
        wait(Some("scan src/: Thinking")).as_ref(),
        wait(Some("scan src/: Running: cargo test")).as_ref(),
    ));
    assert!(!is_phase_transition(
        wait(None).as_ref(),
        wait(Some("scan src/: Thinking")).as_ref(),
    ));
    let task_wait = |task_ids: &[&str], subject: Option<&str>| {
        Some(TurnActivity::Waiting(WaitingReason::TaskOutput {
            task_ids: task_ids.iter().map(|s| s.to_string()).collect(),
            subject: subject.map(str::to_string),
            waits: true,
        }))
    };
    assert!(!is_phase_transition(
        task_wait(&[], None).as_ref(),
        task_wait(&["t-1"], Some("fix flaky test")).as_ref(),
    ));
    assert!(is_phase_transition(
        Some(&TurnActivity::Waiting(WaitingReason::Model)),
        wait(None).as_ref(),
    ));
    assert!(is_phase_transition(
        wait(None).as_ref(),
        task_wait(&[], None).as_ref(),
    ));
    assert!(is_phase_transition(None, wait(None).as_ref()));
    assert!(is_phase_transition(
        wait(Some("scan src/: Thinking")).as_ref(),
        Some(&TurnActivity::Responding),
    ));
    let tool = |title: &str| {
        Some(TurnActivity::ToolRunning {
            title: title.to_string(),
            description: None,
        })
    };
    assert!(is_phase_transition(
        tool("cargo test").as_ref(),
        tool("cargo build").as_ref(),
    ));
    assert!(!is_phase_transition(
        tool("cargo test").as_ref(),
        tool("cargo test").as_ref(),
    ));
    let writing = |name: Option<&str>, ordinal: u32| {
        Some(TurnActivity::WritingToolCall(WritingToolCall {
            tool_name: name.map(str::to_string),
            ordinal: std::num::NonZeroU32::new(ordinal).unwrap(),
        }))
    };
    assert!(!is_phase_transition(
        writing(Some("read_file"), 1).as_ref(),
        writing(Some("write"), 1).as_ref(),
    ));
    assert!(!is_phase_transition(
        writing(Some("read_file"), 1).as_ref(),
        writing(Some("read_file"), 2).as_ref(),
    ));
    assert!(is_phase_transition(
        Some(&TurnActivity::Thinking),
        writing(Some("read_file"), 1).as_ref(),
    ));
    assert!(is_phase_transition(
        writing(Some("read_file"), 1).as_ref(),
        wait(None).as_ref(),
    ));
}
/// A stream that is not zero-based still counts from the first observed
/// call — no "(4)" with no predecessors.
#[test]
fn writing_tool_call_ordinal_ranks_observed_indexes() {
    let mut tracker = AcpUpdateTracker::new();
    let label = |tracker: &AcpUpdateTracker| {
        let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
            panic!("expected WritingToolCall activity");
        };
        writing.label()
    };
    tracker.note_tool_call_arguments_delta(Some("write"), 3);
    assert_eq!(label(&tracker), "Writing file…");
    tracker.note_tool_call_arguments_delta(Some("read_file"), 7);
    assert_eq!(label(&tracker), "Preparing read_file (2)…");
}
/// Streams may emit id/args chunks before `function.name`. A nameless
/// first chunk must still mark its index as observed, so a later sibling
/// ranks after it instead of colliding on the same ordinal.
#[test]
fn writing_tool_call_nameless_delta_still_ranks() {
    let mut tracker = AcpUpdateTracker::new();
    let label = |tracker: &AcpUpdateTracker| {
        let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
            panic!("expected WritingToolCall activity");
        };
        writing.label()
    };
    tracker.note_tool_call_arguments_delta(Some("write"), 0);
    tracker.note_tool_call_arguments_delta(None, 1);
    assert_eq!(label(&tracker), "Preparing tool call (2)…");
    tracker.note_tool_call_arguments_delta(Some("read_file"), 2);
    assert_eq!(label(&tracker), "Preparing read_file (3)…");
    tracker.note_tool_call_arguments_delta(Some("grep"), 1);
    assert_eq!(label(&tracker), "Preparing grep (2)…");
}
#[test]
fn writing_tool_call_interleaved_indexes_restore_names() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let label = |tracker: &AcpUpdateTracker| {
        let Some(TurnActivity::WritingToolCall(writing)) = tracker.activity() else {
            panic!("expected WritingToolCall activity");
        };
        writing.label()
    };
    assert!(tracker.note_tool_call_arguments_delta(Some("write"), 0));
    assert_eq!(label(&tracker), "Writing file…");
    assert!(tracker.note_tool_call_arguments_delta(Some("read_file"), 1));
    assert_eq!(label(&tracker), "Preparing read_file (2)…");
    assert!(tracker.note_tool_call_arguments_delta(None, 0));
    assert_eq!(label(&tracker), "Writing file…");
    assert!(!tracker.note_tool_call_arguments_delta(None, 0));
    assert!(!tracker.note_tool_call_arguments_delta(Some("write"), 0));
    assert!(tracker.note_tool_call_arguments_delta(None, 1));
    assert_eq!(label(&tracker), "Preparing read_file (2)…");
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Read, "read_file"),
        &meta(),
        &mut sb,
    );
    assert!(tracker.note_tool_call_arguments_delta(None, 0));
    assert_eq!(label(&tracker), "Preparing tool call…");
}
#[test]
fn writing_tool_call_cleared_on_finish_turn() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.note_tool_call_arguments_delta(Some("write"), 0);
    tracker.finish_turn(&mut sb);
    assert_eq!(tracker.activity(), None);
}
/// A backgrounded tool keeps streaming stdout `ToolCallUpdate`s that the
/// tracker drops as no-ops (`bg_deferred_tools`). Those must not strip the
/// writing label of the NEXT call's args stream — only the tool's own
/// canonical `ToolCall` or a new text/thought chunk ends that window.
#[test]
fn writing_tool_call_survives_bg_deferred_stdout_update() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `sleep 9999`"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(
        tool_update_in_progress_bg("tc1", b"started"),
        &meta(),
        &mut sb,
    );
    assert!(tracker.bg_deferred_tools.contains_key("tc1"));
    tracker.note_tool_call_arguments_delta(Some("write"), 0);
    assert!(matches!(
        tracker.activity(),
        Some(TurnActivity::WritingToolCall(_))
    ));
    assert!(!tracker.handle_update(
        tool_update_in_progress_bg("tc1", b"more output"),
        &meta(),
        &mut sb,
    ));
    assert!(
        matches!(tracker.activity(), Some(TurnActivity::WritingToolCall(_))),
        "a deferred bg stdout update must not strip the writing label"
    );
}
/// The blocking bg-plumbing tools are kept out of scrollback but the turn
/// IS blocked on them — `activity()` must name the wait instead of the old
/// generic `None` (→ "Waiting…"). Task-output tools only advertise once
/// raw_input proves them blocking (`timeout_ms > 0`); before that the
/// wait is not shown (display mirrors interject eligibility).
#[test]
fn activity_waiting_for_blocking_bg_plumbing_tools() {
    let cases = [
        ("wait_commands_or_subagents", WaitingReason::TasksComplete),
        ("wait_tasks", WaitingReason::TasksComplete),
        ("Await", WaitingReason::Sleep),
        ("Sleep 5s", WaitingReason::Sleep),
    ];
    for (title, expected) in cases {
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        tracker.handle_update(
            tool_call("t1", acp::ToolKind::Other, title),
            &meta(),
            &mut sb,
        );
        assert_eq!(
            tracker.activity(),
            Some(TurnActivity::Waiting(expected.clone())),
            "title {title:?} should produce {expected:?}"
        );
    }
    for title in ["get_command_or_subagent_output", "get_task_output"] {
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        tracker.handle_update(
            tool_call("t1", acp::ToolKind::Other, title),
            &meta(),
            &mut sb,
        );
        assert_eq!(
            tracker.activity(),
            None,
            "{title:?}: unknown blocking-ness must not advertise a wait"
        );
        tracker.handle_update(timeout_update("t1", 30_000), &meta(), &mut sb);
        assert_eq!(
            tracker.activity(),
            Some(TurnActivity::Waiting(WaitingReason::task_output())),
            "{title:?}: known-blocking wait must be named"
        );
    }
}
/// A known-blocking wait must beat an open (residual/pre-created) thought entry.
#[test]
fn activity_known_blocking_wait_outranks_thinking() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(thought_chunk("planning the wait…"), &meta(), &mut sb);
    assert_eq!(tracker.activity(), Some(TurnActivity::Thinking));
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "get_command_or_subagent_output"),
        &meta(),
        &mut sb,
    );
    tracker.pre_create_thinking(&mut sb);
    assert!(
        tracker.current_thinking.is_some(),
        "precondition: residual/pre-created thinking is live"
    );
    tracker.handle_update(timeout_update("t1", 60_000), &meta(), &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::task_output())),
        "known-blocking wait must beat Thinking for the status spinner"
    );
}
/// Thought chunks on the same stream must not erase an in-flight wait.
#[test]
fn thought_chunk_does_not_clear_active_blocking_wait() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let m = meta_stream(42);
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "get_command_or_subagent_output"),
        &m,
        &mut sb,
    );
    tracker.handle_update(timeout_update("t1", 60_000), &m, &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::task_output()))
    );
    tracker.handle_update(thought_chunk("still waiting…"), &m, &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::task_output())),
        "active wait must survive same-stream thought chunks"
    );
}
/// stream_start rollover must not pre-create a thought block during a wait.
#[test]
fn stream_start_does_not_pre_create_thinking_during_blocking_wait() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let m1 = meta_stream(1_000);
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "get_command_or_subagent_output"),
        &m1,
        &mut sb,
    );
    tracker.handle_update(timeout_update("t1", 30_000), &m1, &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::task_output()))
    );
    let m2 = meta_stream(9_999);
    tracker.handle_update(timeout_update("t1", 30_000), &m2, &mut sb);
    assert!(
        tracker.current_thinking.is_none(),
        "must not pre-create thinking while a known-blocking wait is live"
    );
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::task_output()))
    );
}
/// Regression: a resumed thought with no `stream_start_ms` must clear a
/// stale wait (show Thinking, not a stuck wait spinner).
#[test]
fn resumed_thought_without_stream_start_clears_stale_wait() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let m = meta_stream(100);
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "get_command_or_subagent_output"),
        &m,
        &mut sb,
    );
    tracker.handle_update(timeout_update("t1", 60_000), &m, &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::task_output())),
        "precondition: sendable wait is live"
    );
    tracker.handle_update(
        thought_chunk("resuming, let me check the output…"),
        &meta(),
        &mut sb,
    );
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Thinking),
        "resumed-round thought (no stream_start) must clear the stale wait"
    );
}
/// ToolCallUpdate carrying a `timeout_ms` raw_input (the shape the shell
/// sends on the first InProgress update).
fn timeout_update(id: &str, timeout_ms: u64) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from(id)),
        acp::ToolCallUpdateFields::new()
            .raw_input(Some(serde_json::json!({ "timeout_ms": timeout_ms }))),
    ))
}
/// A blocking-wait reason is dropped when the suppressed tool completes, so
/// the spinner stops showing it.
#[test]
fn blocking_wait_cleared_on_tool_completion() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "get_command_or_subagent_output"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(timeout_update("t1", 30_000), &meta(), &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::task_output()))
    );
    tracker.handle_update(tool_update_completed("t1"), &meta(), &mut sb);
    assert_eq!(tracker.activity(), None);
}
/// `finish_turn` clears any lingering blocking-wait state.
#[test]
fn blocking_wait_cleared_by_finish_turn() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "wait_tasks"),
        &meta(),
        &mut sb,
    );
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::TasksComplete))
    );
    tracker.finish_turn(&mut sb);
    assert_eq!(tracker.activity(), None);
}
/// `kill_*` is suppressed but doesn't block the turn — no waiting reason.
#[test]
fn kill_tool_is_not_a_blocking_wait() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "kill_command_or_subagent"),
        &meta(),
        &mut sb,
    );
    assert_eq!(tracker.activity(), None);
}
/// A response stream outranks a still-open blocking wait: show Responding.
#[test]
fn streaming_overrides_blocking_wait() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "get_task_output"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(timeout_update("t1", 30_000), &meta(), &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::task_output()))
    );
    tracker.handle_update(agent_chunk("partial"), &meta(), &mut sb);
    assert_eq!(tracker.activity(), Some(TurnActivity::Responding));
}
/// A same-stream (co-batched) thought must not clear an active wait.
#[test]
fn same_stream_thought_after_wait_tool_keeps_blocking_wait() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let m = meta_stream(100);
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "get_command_or_subagent_output"),
        &m,
        &mut sb,
    );
    tracker.handle_update(
        acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from("t1")),
            acp::ToolCallUpdateFields::new().raw_input(Some(serde_json::json!({
                "task_ids": ["bg-1"],
                "timeout_ms": 180_000,
            }))),
        )),
        &m,
        &mut sb,
    );
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::TaskOutput {
            task_ids: vec!["bg-1".into()],
            subject: None,
            waits: true,
        }))
    );
    tracker.handle_update(thought_chunk("planning next…"), &m, &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::TaskOutput {
            task_ids: vec!["bg-1".into()],
            subject: None,
            waits: true,
        })),
        "same-stream thought must not clear an active task-output wait"
    );
}
/// raw_input with task_ids on the first update populates the wait reason so
/// the view can resolve a display subject from live bg task state.
#[test]
fn task_output_wait_captures_task_ids_from_raw_input_update() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "get_command_or_subagent_output"),
        &meta(),
        &mut sb,
    );
    assert_eq!(tracker.activity(), None);
    tracker.handle_update(
        acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from("t1")),
            acp::ToolCallUpdateFields::new().raw_input(Some(serde_json::json!({
                "task_ids": ["bg-123", "bg-456"],
                "timeout_ms": 30_000,
            }))),
        )),
        &meta(),
        &mut sb,
    );
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::TaskOutput {
            task_ids: vec!["bg-123".into(), "bg-456".into()],
            subject: None,
            waits: true,
        }))
    );
}
/// `waits` derives from raw_input `timeout_ms`: 0/missing are instant
/// polls (not interject-eligible); only >0 marks a blocking wait.
#[test]
fn task_output_waits_tracks_timeout_ms() {
    let tc = |raw: Option<serde_json::Value>| {
        acp::ToolCall::new(
            acp::ToolCallId::new(std::sync::Arc::from("w")),
            "get_task_output",
        )
        .kind(acp::ToolKind::Other)
        .status(acp::ToolCallStatus::Pending)
        .content(vec![])
        .raw_input(raw)
        .locations(vec![])
    };
    let waits = |raw| match blocking_wait_reason(&tc(raw)) {
        Some(WaitingReason::TaskOutput { waits, .. }) => waits,
        other => panic!("expected TaskOutput, got {other:?}"),
    };
    assert!(!waits(None), "missing raw_input defaults to instant poll");
    assert!(!waits(Some(serde_json::json!({ "task_ids": ["a"] }))));
    assert!(!waits(Some(
        serde_json::json!({ "task_ids": ["a"], "timeout_ms": 0 })
    )));
    assert!(waits(Some(
        serde_json::json!({ "task_ids": ["a"], "timeout_ms": 1 })
    )));
}
#[test]
fn waiting_reason_label_uses_subject_when_present() {
    assert_eq!(
        WaitingReason::TaskOutput {
            task_ids: vec!["t1".into()],
            subject: Some("compile release".into()),
            waits: false,
        }
        .label(),
        "compile release…"
    );
    assert_eq!(
        WaitingReason::task_output().label(),
        "Waiting on task output…"
    );
    assert_eq!(
        WaitingReason::TaskOutput {
            task_ids: vec![],
            subject: Some("\n  first line  \nsecond".into()),
            waits: false,
        }
        .label(),
        "first line…"
    );
    let long = "x".repeat(80);
    let label = WaitingReason::TaskOutput {
        task_ids: vec![],
        subject: Some(long),
        waits: false,
    }
    .label();
    assert!(label.ends_with('…'));
    let inner = label.strip_suffix('…').unwrap();
    assert_eq!(inner.chars().count(), MAX_ACTIVITY_SUBJECT_CHARS);
}
#[test]
fn format_waiting_for_subject_matches_label_shape() {
    assert_eq!(format_waiting_for_subject("run tests"), "run tests…");
    assert_eq!(format_waiting_for_subject("   "), "Waiting on task output…");
}
/// A `task` ToolCall carrying the shell's `_meta.subagentBackground` flag.
fn task_call_with_bg(id: &str, background: bool) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(acp::ToolCallId::new(Arc::from(id)), "task".to_string())
            .kind(acp::ToolKind::Other)
            .status(acp::ToolCallStatus::Pending)
            .content(vec![])
            .locations(vec![])
            .meta(Some(
                [(
                    "subagentBackground".to_string(),
                    serde_json::Value::Bool(background),
                )]
                .into_iter()
                .collect(),
            )),
    )
}
/// Shell-stamped foreground (`subagentBackground=false`): the subagent wait
/// surfaces from frame 1 — no "Waiting for response…" flash.
#[test]
fn foreground_stamp_waits_on_subagent_from_frame_one() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(task_call_with_bg("t1", false), &meta(), &mut sb);
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::subagent())),
        "a foreground-stamped subagent spawn surfaces the wait immediately"
    );
}
/// Shell-stamped background (`subagentBackground=true`, the default): the
/// model keeps working, so no subagent wait surfaces — not even a one-frame
/// flash.
#[test]
fn background_stamp_never_surfaces_subagent_wait() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(task_call_with_bg("t1", true), &meta(), &mut sb);
    assert_eq!(
        tracker.activity(),
        None,
        "a background-stamped subagent spawn must not surface any wait"
    );
}
/// Older shell with no `subagentBackground` stamp: fall back to the
/// provisional foreground assumption (the refinement update still drops it
/// for a background spawn).
#[test]
fn foreground_task_waits_on_subagent_immediately() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "task"),
        &meta(),
        &mut sb,
    );
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::subagent()))
    );
    tracker.finish_turn(&mut sb);
    assert_eq!(tracker.activity(), None);
}
/// A background subagent doesn't block the parent: once an update reveals
/// `run_in_background`, the provisional subagent wait is dropped.
#[test]
fn background_task_clears_subagent_wait() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("t1", acp::ToolKind::Other, "task"),
        &meta(),
        &mut sb,
    );
    assert_eq!(
        tracker.activity(),
        Some(TurnActivity::Waiting(WaitingReason::subagent()))
    );
    let bg_update = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from("t1")),
        acp::ToolCallUpdateFields::new().raw_input(Some(serde_json::json!({
            "variant": "Task",
            "task_id": "sa1",
            "run_in_background": true
        }))),
    ));
    tracker.handle_update(bg_update, &meta(), &mut sb);
    assert_eq!(tracker.activity(), None);
}
fn available_commands_update(names: &[&str]) -> acp::SessionUpdate {
    acp::SessionUpdate::AvailableCommandsUpdate(acp::AvailableCommandsUpdate::new(
        names
            .iter()
            .map(|n| acp::AvailableCommand::new(n.to_string(), format!("{n} command")))
            .collect(),
    ))
}
#[test]
fn tracker_captures_available_commands_update() {
    let mut tracker = AcpUpdateTracker::new();
    let mut sb = ScrollbackState::new();
    assert!(tracker.take_pending_acp_commands().is_none());
    let changed = tracker.handle_update(
        available_commands_update(&["flush", "compact"]),
        &meta(),
        &mut sb,
    );
    assert!(changed, "AvailableCommandsUpdate should signal redraw");
    let cmds = tracker
        .take_pending_acp_commands()
        .expect("should have pending");
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].name, "flush");
    assert_eq!(cmds[1].name, "compact");
}
#[test]
fn tracker_single_drain_clears_pending() {
    let mut tracker = AcpUpdateTracker::new();
    let mut sb = ScrollbackState::new();
    tracker.handle_update(available_commands_update(&["flush"]), &meta(), &mut sb);
    assert!(tracker.take_pending_acp_commands().is_some());
    assert!(tracker.take_pending_acp_commands().is_none());
}
#[test]
fn tracker_latest_update_replaces_pending() {
    let mut tracker = AcpUpdateTracker::new();
    let mut sb = ScrollbackState::new();
    tracker.handle_update(available_commands_update(&["old"]), &meta(), &mut sb);
    tracker.handle_update(
        available_commands_update(&["new_a", "new_b"]),
        &meta(),
        &mut sb,
    );
    let cmds = tracker
        .take_pending_acp_commands()
        .expect("should have pending");
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].name, "new_a");
    assert_eq!(cmds[1].name, "new_b");
}
#[test]
fn parse_search_tool_results_grouped_format() {
    let json = serde_json::json!({
        "results": [
            {
                "server": "linear",
                "tools": [
                    {
                        "tool_name": "linear__save_issue",
                        "description": "Create an issue",
                        "score": 0.8,
                        "parameters": ["stale_param_a", "stale_param_b"],
                        "input_schema": {"type": "object", "properties": {"title": {"type": "string"}, "team": {"type": "string"}}, "required": ["title"]}
                    },
                    {
                        "tool_name": "linear__list_issues",
                        "description": "List issues",
                        "score": 0.5,
                        "parameters": ["stale_query"],
                        "input_schema": {"type": "object", "properties": {"query": {"type": "string"}}}
                    }
                ]
            },
            {
                "server": "slack",
                "tools": [
                    {
                        "tool_name": "slack__send_message",
                        "description": "Send a message",
                        "score": 0.3,
                        "input_schema": {}
                    }
                ]
            }
        ],
        "total_hidden_tools": 10,
        "status": "ready"
    });
    let content = serde_json::to_string_pretty(&json).unwrap();
    let results = parse_search_tool_results(&content);
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].name, "linear__save_issue");
    assert_eq!(results[0].server, "linear");
    assert_eq!(results[0].description, "Create an issue");
    assert!((results[0].score - 0.8).abs() < f64::EPSILON);
    assert_eq!(results[1].name, "linear__list_issues");
    assert_eq!(results[1].server, "linear");
    assert_eq!(results[2].name, "slack__send_message");
    assert_eq!(results[2].server, "slack");
}
#[test]
fn parse_search_tool_results_old_flat_format_returns_empty() {
    let json = serde_json::json!({
        "results": [
            {
                "tool_name": "linear__save_issue",
                "server_name": "linear",
                "description": "Create an issue",
                "score": 0.8
            }
        ]
    });
    let content = serde_json::to_string_pretty(&json).unwrap();
    let results = parse_search_tool_results(&content);
    assert!(
        results.is_empty(),
        "old flat format should not parse: {results:?}"
    );
}
#[test]
fn tracker_extracts_tools_meta_from_available_commands_update() {
    let mut tracker = AcpUpdateTracker::new();
    let mut sb = ScrollbackState::new();
    let update = acp::SessionUpdate::AvailableCommandsUpdate(
        acp::AvailableCommandsUpdate::new(vec![acp::AvailableCommand::new(
            "loop".to_string(),
            "loop".to_string(),
        )])
        .meta(
            serde_json::json!({"tools": ["scheduler_create", "read_file"]})
                .as_object()
                .cloned(),
        ),
    );
    tracker.handle_update(update, &meta(), &mut sb);
    let tools = tracker
        .take_pending_acp_tools()
        .expect("tools list should be present");
    assert_eq!(tools, vec!["scheduler_create", "read_file"]);
    assert!(tracker.take_pending_acp_tools().is_none());
}
#[test]
fn tracker_tools_meta_absent_when_meta_missing() {
    let mut tracker = AcpUpdateTracker::new();
    let mut sb = ScrollbackState::new();
    tracker.handle_update(available_commands_update(&["loop"]), &meta(), &mut sb);
    assert!(tracker.take_pending_acp_tools().is_none());
    assert!(tracker.take_pending_acp_commands().is_some());
}
#[test]
fn parse_tools_meta_handles_shape_variants() {
    assert_eq!(
        parse_tools_meta(serde_json::json!({"tools": ["a", "b"]}).as_object()),
        Some(vec!["a".to_string(), "b".to_string()]),
    );
    assert_eq!(parse_tools_meta(None), None);
    assert_eq!(
        parse_tools_meta(serde_json::json!({"other": 1}).as_object()),
        None,
    );
    assert_eq!(
        parse_tools_meta(serde_json::json!({"tools": "nope"}).as_object()),
        None,
    );
    assert_eq!(
        parse_tools_meta(serde_json::json!({"tools": ["a", 1, true, "b"]}).as_object()),
        Some(vec!["a".to_string(), "b".to_string()]),
    );
}
#[test]
fn update_summary_is_compact_for_huge_tool_output() {
    let big = serde_json::to_value(vec![0u8; 100_000]).unwrap();
    let update = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from("call-1")),
        acp::ToolCallUpdateFields::new()
            .status(Some(acp::ToolCallStatus::InProgress))
            .raw_output(Some(big)),
    ));
    let summary = update_summary(&update);
    assert!(
        summary.len() < 200,
        "summary must not scale with payload: {} bytes",
        summary.len()
    );
    assert!(summary.contains("tool_call_update"), "{summary}");
    assert!(summary.contains("id=call-1"), "{summary}");
    assert!(summary.contains("arr(100000)"), "{summary}");
}
#[test]
fn update_summary_reports_chunk_text_size() {
    let summary = update_summary(&agent_chunk(&"x".repeat(5000)));
    assert!(summary.contains("agent_message_chunk"), "{summary}");
    assert!(summary.contains("text=5000B"), "{summary}");
    assert!(summary.len() < 100, "{summary}");
}
#[test]
fn json_size_hint_shapes() {
    assert_eq!(json_size_hint(&serde_json::json!(null)), "null");
    assert_eq!(json_size_hint(&serde_json::json!("abcd")), "str(4B)");
    assert_eq!(json_size_hint(&serde_json::json!([1, 2, 3])), "arr(3)");
    assert_eq!(
        json_size_hint(&serde_json::json!({"output": [1, 2], "cmd": "ls"})),
        "obj(2 keys, ~4B)"
    );
}
#[test]
fn meta_summary_handles_missing_fields() {
    assert_eq!(
        meta_summary(&NotificationMeta::default()),
        "seq=- tokens=- prompt=- stream_start=-"
    );
    let m = NotificationMeta {
        event_seq: Some(42),
        total_tokens: Some(1234),
        ..Default::default()
    };
    assert_eq!(
        meta_summary(&m),
        "seq=42 tokens=1234 prompt=- stream_start=-"
    );
}
#[test]
fn build_and_parse_tools_meta_round_trip() {
    let names = vec!["scheduler_create".to_string(), "image_gen".to_string()];
    let wire = serde_json::json!({ "tools": names });
    assert_eq!(parse_tools_meta(wire.as_object()), Some(names));
}
#[test]
fn tracker_finish_turn_does_not_clear_pending_acp_commands() {
    let mut tracker = AcpUpdateTracker::new();
    let mut sb = ScrollbackState::new();
    tracker.handle_update(available_commands_update(&["persist"]), &meta(), &mut sb);
    tracker.finish_turn(&mut sb);
    assert!(tracker.take_pending_acp_commands().is_some());
}
#[test]
fn tracker_finish_turn_does_not_clear_pending_acp_tools() {
    let mut tracker = AcpUpdateTracker::new();
    let mut sb = ScrollbackState::new();
    let update = acp::SessionUpdate::AvailableCommandsUpdate(
        acp::AvailableCommandsUpdate::new(vec![acp::AvailableCommand::new(
            "loop".to_string(),
            "loop".to_string(),
        )])
        .meta(
            serde_json::json!({"tools": ["scheduler_create"]})
                .as_object()
                .cloned(),
        ),
    );
    tracker.handle_update(update, &meta(), &mut sb);
    tracker.finish_turn(&mut sb);
    let tools = tracker
        .take_pending_acp_tools()
        .expect("pending_acp_tools should survive finish_turn");
    assert_eq!(tools, vec!["scheduler_create"]);
}
#[test]
fn tracker_meta_less_update_preserves_prior_pending_acp_tools() {
    let mut tracker = AcpUpdateTracker::new();
    let mut sb = ScrollbackState::new();
    let with_tools = acp::SessionUpdate::AvailableCommandsUpdate(
        acp::AvailableCommandsUpdate::new(vec![]).meta(
            serde_json::json!({"tools": ["scheduler_create"]})
                .as_object()
                .cloned(),
        ),
    );
    tracker.handle_update(with_tools, &meta(), &mut sb);
    tracker.handle_update(available_commands_update(&["loop"]), &meta(), &mut sb);
    let tools = tracker
        .take_pending_acp_tools()
        .expect("prior pending tools should be preserved");
    assert_eq!(tools, vec!["scheduler_create"]);
}
/// Build a `ToolCall` that mimics the initial ACP register-early payload
/// emitted by `acp_session.rs`: title comes from the model's function
/// name, raw_input is None.
fn initial_tool_call(id: &str, function_name: &str) -> acp::ToolCall {
    acp::ToolCall::new(
        acp::ToolCallId::new(Arc::from(id)),
        function_name.to_string(),
    )
    .kind(acp::ToolKind::Other)
    .status(acp::ToolCallStatus::Pending)
}
#[test]
fn is_task_tool_recognizes_echo_agent_build_variant() {
    assert!(is_task_tool(&initial_tool_call("tc1", "task")));
    let mut with_variant = initial_tool_call("tc2", "anything");
    with_variant.raw_input = Some(serde_json::json!({"variant": "Task"}));
    assert!(is_task_tool(&with_variant));
}
#[test]
fn is_task_tool_rejects_unrelated_tools() {
    assert!(!is_task_tool(&initial_tool_call("tc1", "read_file")));
    assert!(!is_task_tool(&initial_tool_call("tc2", "Read")));
    assert!(!is_task_tool(&initial_tool_call("tc3", "todo_write")));
    let mut with_variant = initial_tool_call("tc4", "anything");
    with_variant.raw_input = Some(serde_json::json!({"variant": "Bash"}));
    assert!(!is_task_tool(&with_variant));
}
#[test]
fn is_bg_plumbing_tool_recognizes_all_name_generations() {
    assert!(is_bg_plumbing_tool(&initial_tool_call(
        "t1",
        "get_command_or_subagent_output"
    )));
    assert!(is_bg_plumbing_tool(&initial_tool_call(
        "t2",
        "kill_command_or_subagent"
    )));
    assert!(is_bg_plumbing_tool(&initial_tool_call(
        "t3",
        "wait_commands_or_subagents"
    )));
    assert!(is_bg_plumbing_tool(&initial_tool_call(
        "t4",
        "get_task_output"
    )));
    assert!(is_bg_plumbing_tool(&initial_tool_call("t5", "kill_task")));
    assert!(is_bg_plumbing_tool(&initial_tool_call("t6", "wait_tasks")));
    assert!(is_bg_plumbing_tool(&initial_tool_call(
        "t7",
        "get_task_or_subagent_output"
    )));
    assert!(is_bg_plumbing_tool(&initial_tool_call(
        "t8",
        "kill_task_or_subagent"
    )));
    assert!(is_bg_plumbing_tool(&initial_tool_call(
        "t9",
        "wait_tasks_or_subagents"
    )));
    assert!(is_bg_plumbing_tool(&initial_tool_call("t10", "AwaitShell")));
    assert!(is_bg_plumbing_tool(&initial_tool_call("t10b", "Await")));
    let mut with_variant = initial_tool_call("t11", "anything");
    with_variant.raw_input = Some(serde_json::json!({"variant": "WaitTasks"}));
    assert!(is_bg_plumbing_tool(&with_variant));
    assert!(!is_bg_plumbing_tool(&initial_tool_call("t12", "read_file")));
    assert!(!is_bg_plumbing_tool(&initial_tool_call(
        "t13",
        "spawn_subagent"
    )));
}
#[test]
fn pascal_case_task_tool_call_is_suppressed_from_scrollback() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Other, "Task"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 0, "PascalCase Task tool must be suppressed");
    assert!(tracker.suppressed_tools.contains("tc1"));
    tracker.handle_update(tool_update_completed("tc1"), &meta(), &mut sb);
    assert_eq!(
        sb.len(),
        0,
        "PascalCase Task updates must also be suppressed"
    );
}
#[test]
fn task_tool_surfaces_as_subagent_wait_not_run_task() {
    for title in ["task", "Task"] {
        let mut sb = ScrollbackState::new();
        let mut tracker = AcpUpdateTracker::new();
        tracker.handle_update(
            tool_call("tc1", acp::ToolKind::Other, title),
            &meta(),
            &mut sb,
        );
        let activity = tracker.activity();
        assert!(
            !matches!(activity, Some(TurnActivity::ToolRunning { .. })),
            "suppressed task tool with title={title:?} must not surface as ToolRunning \
             (would render as 'Run {title}' in the bottom turn-status spinner)"
        );
        assert_eq!(
            activity,
            Some(TurnActivity::Waiting(WaitingReason::subagent())),
            "suppressed task tool with title={title:?} should surface as the subagent wait"
        );
    }
}
/// Helper: create an InProgress ToolCallUpdate with raw_input containing is_background.
fn tool_update_in_progress_bg(id: &str, output_bytes: &[u8]) -> acp::SessionUpdate {
    use echo_agent_tools::types::output::{BashOutput, ToolOutput};
    let bash = BashOutput {
        output: output_bytes.to_vec(),
        output_for_prompt: String::new(),
        exit_code: 0,
        command: String::new(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: None,
        current_dir: String::new(),
        output_file: String::new(),
        total_bytes: output_bytes.len(),
        output_delta: None,
        was_bare_echo: false,
    };
    acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from(id)),
        acp::ToolCallUpdateFields::new()
            .status(Some(acp::ToolCallStatus::InProgress))
            .raw_output(serde_json::to_value(ToolOutput::Bash(bash)).ok())
            .raw_input(Some(serde_json::json!({
                "command": "sleep 9999",
                "is_background": true,
                "description": "long running task"
            }))),
    ))
}
/// Regression: is_bg_tool() detected on first InProgress defers the tool
/// before any scrollback entry is created.
#[test]
fn bg_tool_detected_at_first_update_defers_to_bg() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `sleep 9999`"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    assert_eq!(tracker.pending_tools.len(), 1);
    let output_epoch = tracker.agent_output_epoch;
    let modified = tracker.handle_update(
        tool_update_in_progress_bg("tc1", b"started"),
        &meta(),
        &mut sb,
    );
    assert!(
        !modified,
        "bg tool deferral should suppress further output streaming"
    );
    assert_eq!(
        tracker.agent_output_epoch, output_epoch,
        "deferral must not bump the epoch (it is not visible agent output)"
    );
    assert_eq!(sb.len(), 1, "real execute entry kept for demotion");
    assert!(
        !tracker.pending_tools.is_empty(),
        "tool stays in pending_tools for demotion entry_id"
    );
    assert!(
        tracker.bg_deferred_tools.contains_key("tc1"),
        "tool should be added to bg_deferred_tools"
    );
    assert_eq!(
        tracker.bg_deferred_tools.get("tc1").unwrap().as_deref(),
        Some("long running task"),
        "description should be extracted from raw_input"
    );
}
/// Regression: a bg-tool deferral (here dropping the placeholder row) must
/// not bump `agent_output_epoch` — it is not visible agent output.
#[test]
fn bg_tool_deferral_does_not_bump_agent_output_epoch() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Other, "run_terminal_command"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    let epoch = tracker.agent_output_epoch;
    let update = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from("tc1")),
        acp::ToolCallUpdateFields::new()
            .status(Some(acp::ToolCallStatus::InProgress))
            .raw_input(Some(serde_json::json!({
                "is_background": true,
                "description": "long running task"
            }))),
    ));
    assert!(!tracker.handle_update(update, &meta(), &mut sb));
    assert_eq!(sb.len(), 0, "placeholder dropped on deferral");
    assert!(tracker.bg_deferred_tools.contains_key("tc1"));
    assert_eq!(
        tracker.agent_output_epoch, epoch,
        "deferral must not bump the epoch (it is not visible agent output)"
    );
}
/// Eager kind=Other title=`run_terminal_command` must not flash in the TUI.
#[test]
fn eager_execute_function_name_is_loading_placeholder_not_label() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Other, "run_terminal_command"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    match &sb.get(0).unwrap().block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(ex)) => {
            assert!(
                ex.command.is_empty(),
                "must not use function name as command: {:?}",
                ex.command
            );
        }
        other => panic!("expected loading Execute, got {other:?}"),
    }
    let _ = tracker.handle_update(tool_update_in_progress_bg("tc1", b""), &meta(), &mut sb);
    assert_eq!(sb.len(), 1, "real command keeps entry for demotion");
    match &sb.get(0).unwrap().block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(ex)) => {
            assert_eq!(ex.command, "sleep 9999");
            assert_eq!(ex.description.as_deref(), Some("long running task"));
        }
        other => panic!("expected refined Execute, got {other:?}"),
    }
    assert!(tracker.bg_deferred_tools.contains_key("tc1"));
    assert!(tracker.pending_tools.contains_key("tc1"));
}
/// `raw_input.command: ""` must still map Other+function-name to loading Execute
/// (not leave a bold `run_terminal_command` Other label).
#[test]
fn empty_command_key_still_maps_function_name_to_loading_execute() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Other, "run_terminal_command"),
        &meta(),
        &mut sb,
    );
    let update = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from("tc1")),
        acp::ToolCallUpdateFields::new()
            .status(Some(acp::ToolCallStatus::InProgress))
            .raw_input(Some(serde_json::json!({
                "command": "",
                "description": "still loading"
            }))),
    ));
    tracker.handle_update(update, &meta(), &mut sb);
    assert_eq!(sb.len(), 1);
    match &sb.get(0).unwrap().block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(ex)) => {
            assert!(ex.command.is_empty(), "empty command stays placeholder");
            assert_eq!(ex.description.as_deref(), Some("still loading"));
        }
        other => panic!("expected loading Execute, got {other:?}"),
    }
}
/// Real backgrounded `bash` command must not be treated as a placeholder.
#[test]
fn real_bash_command_is_not_dropped_on_bg_deferral() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "bash"),
        &meta(),
        &mut sb,
    );
    let update = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from("tc1")),
        acp::ToolCallUpdateFields::new()
            .status(Some(acp::ToolCallStatus::InProgress))
            .kind(Some(acp::ToolKind::Execute))
            .raw_input(Some(serde_json::json!({
                "command": "bash",
                "is_background": true,
                "description": "start a shell"
            }))),
    ));
    tracker.handle_update(update, &meta(), &mut sb);
    assert_eq!(
        sb.len(),
        1,
        "real command=bash must be kept for demotion, not dropped as placeholder"
    );
    assert!(tracker.pending_tools.contains_key("tc1"));
    assert!(tracker.bg_deferred_tools.contains_key("tc1"));
}
/// A completed function-name `Other` tool call that still carries BashOutput
/// must preserve the command output + exit-code error (mirror of the Execute
/// arm), not drop it when the kind was never refined to Execute.
#[test]
fn completed_other_function_name_preserves_bash_output() {
    use echo_agent_tools::types::output::{BashOutput, ToolOutput};
    let bash = BashOutput {
        output: b"hello from bg\n".to_vec(),
        output_for_prompt: String::new(),
        exit_code: 3,
        command: "echo hi".to_string(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: None,
        current_dir: "/tmp".to_string(),
        output_file: String::new(),
        total_bytes: 14,
        output_delta: None,
        was_bare_echo: false,
    };
    let tc = acp::ToolCall::new(
        acp::ToolCallId::new(Arc::from("tc1")),
        "run_terminal_command".to_string(),
    )
    .kind(acp::ToolKind::Other)
    .status(acp::ToolCallStatus::Completed)
    .content(vec![])
    .raw_input(Some(serde_json::json!({ "command": "echo hi" })))
    .raw_output(serde_json::to_value(ToolOutput::Bash(bash)).ok())
    .locations(vec![]);
    match tool_call_to_block(&tc, None) {
        RenderBlock::ToolCall(ToolCallBlock::Execute(ex)) => {
            assert_eq!(ex.command, "echo hi");
            assert_eq!(ex.output.as_deref(), Some("hello from bg\n"));
            assert_eq!(ex.error.as_deref(), Some("exit code 3"));
        }
        other => panic!("expected Execute with output, got {other:?}"),
    }
}
/// Regression: when raw_input with is_background=true arrives after the
/// Execute block was already created (late detection), the tool must still
/// be moved to bg_deferred_tools so the task_backgrounded handler can
/// demote the existing entry.
#[test]
fn bg_tool_late_detection_defers_existing_entry() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `sleep 9999`"),
        &meta(),
        &mut sb,
    );
    assert_eq!(tracker.pending_tools.len(), 1);
    tracker.handle_update(
        tool_update_in_progress("tc1", b"early output"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1, "Execute block should be created");
    assert!(
        tracker.pending_tools.get("tc1").unwrap().entry_id.is_some(),
        "entry_id should be set"
    );
    let modified = tracker.handle_update(
        tool_update_in_progress_bg("tc1", b"more output"),
        &meta(),
        &mut sb,
    );
    assert!(!modified, "late bg detection should suppress the update");
    assert!(
        tracker.pending_tools.contains_key("tc1"),
        "tool must stay in pending_tools so demotion handler can find entry_id"
    );
    assert!(
        tracker.bg_deferred_tools.contains_key("tc1"),
        "tool should also be in bg_deferred_tools to suppress future updates"
    );
    assert_eq!(sb.len(), 1);
}
/// Non-background Execute tools are unaffected by the late-detection path.
#[test]
fn non_bg_execute_unaffected_by_late_detection() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `ls`"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(
        tool_update_in_progress("tc1", b"file1.rs"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    assert_eq!(tracker.pending_tools.len(), 1);
    assert!(tracker.bg_deferred_tools.is_empty());
    tracker.handle_update(
        tool_update_in_progress("tc1", b"file1.rs\nfile2.rs"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1, "still one entry");
    assert_eq!(tracker.pending_tools.len(), 1, "still pending");
    assert!(
        tracker.bg_deferred_tools.is_empty(),
        "should not defer non-bg tool"
    );
}
/// Regression: handle_user_message must finish_running on pending tool entries
/// before clearing them, otherwise Execute blocks are orphaned as "running".
#[test]
fn handle_user_message_finishes_pending_tool_entries() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `sleep 999`"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(
        tool_update_in_progress("tc1", b"waiting..."),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    assert!(sb.get(0).unwrap().is_running, "tool should be running");
    assert_eq!(tracker.pending_tools.len(), 1);
    tracker.handle_update(user_message("cancel that"), &meta(), &mut sb);
    assert!(
        !sb.get(0).unwrap().is_running,
        "Execute block should be finished by handle_user_message",
    );
    assert!(
        tracker.pending_tools.is_empty(),
        "pending_tools should be drained",
    );
    assert!(
        !sb.needs_animation(),
        "no entries should be animating after user message",
    );
}
/// A send-now interrupt must not finalize the freshly armed page-flip pin.
#[test]
fn handle_user_message_does_not_finalize_fresh_pin() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    for i in 0..30 {
        sb.push_block(RenderBlock::agent_message(format!("history {i}")));
    }
    sb.push_block(RenderBlock::user_prompt("new question"));
    let prompt_idx = sb.len() - 1;
    sb.prepare_layout(80, 8);
    sb.follow_new_turn(Some(prompt_idx), true);
    sb.prepare_layout(80, 8);
    assert!(sb.is_pin_reserve_active(), "pin armed for the new turn");
    assert!(
        !sb.is_pin_reserve_after_turn(),
        "fresh pin is not finalized"
    );
    tracker.handle_update(agent_chunk("responding..."), &meta(), &mut sb);
    tracker.handle_update(user_message("interrupt"), &meta(), &mut sb);
    assert!(
        sb.is_pin_reserve_active(),
        "the interrupt must not drop the pin"
    );
    assert!(
        !sb.is_pin_reserve_after_turn(),
        "a send-now must not finalize the fresh pin (that blocks the overflow chase)"
    );
}
/// Regression: finish_turn must call finish_running even for tools that are
/// in bg_deferred_tools. The turn is over — the original Execute block must
/// not stay orphaned as "running".
#[test]
fn finish_turn_finishes_bg_deferred_tool_entries() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        tool_call("tc1", acp::ToolKind::Execute, "Execute `long cmd`"),
        &meta(),
        &mut sb,
    );
    tracker.handle_update(
        tool_update_in_progress("tc1", b"early output"),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    assert!(sb.get(0).unwrap().is_running);
    tracker.handle_update(
        tool_update_in_progress_bg("tc1", b"more output"),
        &meta(),
        &mut sb,
    );
    assert!(
        tracker.bg_deferred_tools.contains_key("tc1"),
        "tool should be in bg_deferred_tools",
    );
    assert!(
        tracker.pending_tools.contains_key("tc1"),
        "tool should still be in pending_tools",
    );
    tracker.finish_turn(&mut sb);
    assert!(
        !sb.get(0).unwrap().is_running,
        "Execute block must be finished even when in bg_deferred_tools",
    );
    assert!(
        tracker.pending_tools.is_empty(),
        "pending_tools should be drained",
    );
    assert!(
        !sb.needs_animation(),
        "no entries should be animating after finish_turn",
    );
    assert!(
        tracker.bg_deferred_tools.contains_key("tc1"),
        "bg_deferred_tools must survive finish_turn",
    );
}
/// Helper: create a UserMessageChunk with displayText in content block meta.
fn user_message_with_display_text(
    raw_text: &str,
    display_text: &str,
    display_as_skill: bool,
) -> acp::SessionUpdate {
    let mut meta_map = serde_json::Map::new();
    meta_map.insert(
        "displayText".into(),
        serde_json::Value::String(display_text.into()),
    );
    if display_as_skill {
        meta_map.insert("displayAsSkill".into(), serde_json::Value::Bool(true));
    }
    acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
        acp::TextContent::new(raw_text.to_string())
            .meta(serde_json::Value::Object(meta_map).as_object().cloned()),
    )))
}
/// Replay with displayText in content meta shows clean display text
/// instead of raw skill instructions.
#[test]
fn replay_display_text_override() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let raw = "# /loop -- schedule a recurring prompt\n\nParse the input below...";
    tracker.handle_update(
        user_message_with_display_text(raw, "/loop 1m print current time", true),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::UserPrompt(block) => {
            assert_eq!(
                block.skill_token_ranges,
                vec![0..5],
                "leading /loop token styled as skill"
            );
            assert_eq!(block.text, "/loop 1m print current time");
        }
        other => panic!("expected UserPrompt, got {:?}", other),
    }
}
/// displayText with displayAsSkill=false creates a regular prompt block.
#[test]
fn replay_display_text_non_skill() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let raw = "some raw wire content";
    tracker.handle_update(
        user_message_with_display_text(raw, "clean display text", false),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::UserPrompt(block) => {
            assert!(
                block.skill_token_ranges.is_empty(),
                "should NOT be styled as skill"
            );
            assert_eq!(block.text, "clean display text");
        }
        other => panic!("expected UserPrompt, got {:?}", other),
    }
}
/// displayText with legacy XML raw text still skips the body block.
#[test]
fn replay_display_text_with_legacy_xml_skips_body() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let xml = "<command-name>implement</command-name>\n\
                <command-message>/implement</command-message>\n\
                <command-args>fix bug</command-args>";
    tracker.handle_update(
        user_message_with_display_text(xml, "/implement fix bug", true),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::UserPrompt(block) => {
            assert_eq!(block.skill_token_ranges, vec![0..10]);
            assert_eq!(block.text, "/implement fix bug");
        }
        other => panic!("expected UserPrompt, got {:?}", other),
    }
    assert!(
        !tracker.handle_update(user_message("You are an orchestrator..."), &meta(), &mut sb),
        "skill body should be absorbed",
    );
    assert_eq!(sb.len(), 1, "no new entry for skill body");
}
/// Sessions without displayText still work via legacy fallback.
#[test]
fn replay_without_display_text_uses_legacy_detection() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let xml = "<command-name>commit</command-name>\n\
                <command-message>/commit</command-message>\n\
                <command-args>fix typo</command-args>";
    tracker.handle_update(user_message(xml), &meta(), &mut sb);
    assert_eq!(sb.len(), 1);
    let entry = sb.get(0).unwrap();
    match &entry.block {
        RenderBlock::UserPrompt(block) => {
            assert_eq!(block.skill_token_ranges, vec![0..7]);
            assert_eq!(block.text, "/commit fix typo");
        }
        other => panic!("expected UserPrompt, got {:?}", other),
    }
    let mut sb2 = ScrollbackState::new();
    let mut tracker2 = AcpUpdateTracker::new();
    tracker2.handle_update(user_message("/help"), &meta(), &mut sb2);
    let entry2 = sb2.get(0).unwrap();
    match &entry2.block {
        RenderBlock::UserPrompt(block) => {
            assert_eq!(block.skill_token_ranges, vec![0..5]);
            assert_eq!(block.text, "/help");
        }
        other => panic!("expected UserPrompt, got {:?}", other),
    }
}
fn user_message_with_chunk_meta(text: &str, chunk_meta: acp::Meta) -> acp::SessionUpdate {
    acp::SessionUpdate::UserMessageChunk(
        acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
            text.to_string(),
        )))
        .meta(Some(chunk_meta)),
    )
}
fn meta_with_prompt_id(prompt_id: &str) -> NotificationMeta {
    let mut m = meta();
    m.prompt_id = Some(prompt_id.to_string());
    m
}
/// Scrollback hide is type-driven: chunk meta `hideFromScrollback` or
/// notification `promptId` → [`PromptOrigin::hide_user_echo_from_scrollback`].
#[test]
fn replay_hides_user_echo_by_origin_type() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let monitor_xml = "\
<monitor-event task_id=\"019e0000-0000-7000-8000-000000000001\">\n\
[CI] IN_PROGRESS=5 SUCCESS=38\n\
</monitor-event>";
    let mut hide_meta = acp::Meta::new();
    hide_meta.insert("hideFromScrollback".into(), serde_json::json!(true));
    assert!(
        !tracker.handle_update(
            user_message_with_chunk_meta(monitor_xml, hide_meta),
            &meta(),
            &mut sb,
        ),
        "hideFromScrollback meta must suppress regardless of text shape"
    );
    assert!(
        !tracker.handle_update(
            user_message("arbitrary model-only body"),
            &meta_with_prompt_id("task-completed-bg-1"),
            &mut sb,
        ),
        "task-completed origin must suppress via promptId"
    );
    assert!(
        !tracker.handle_update(
            user_message("drain body"),
            &meta_with_prompt_id("notifications-019e0000"),
            &mut sb,
        ),
        "notification-drain origin must suppress via promptId"
    );
    assert!(
        tracker.handle_update(
            user_message("please check the CI status"),
            &meta_with_prompt_id("scheduler-fired-abc"),
            &mut sb,
        ),
        "scheduler-fired must still render (cron path is separate)"
    );
    assert!(
        tracker.handle_update(user_message("please check the CI status"), &meta(), &mut sb),
        "real user text must still render"
    );
    assert_eq!(sb.len(), 2);
    assert!(
        !tracker.handle_update(user_message(monitor_xml), &meta(), &mut sb),
        "legacy untyped monitor XML still suppressed"
    );
    assert!(
        !tracker.handle_update(
            user_message("<system-reminder>\nBackground task done.\n</system-reminder>"),
            &meta(),
            &mut sb,
        ),
        "legacy system-reminder still suppressed"
    );
    let batched = "2 monitor events from 1 monitor (use get_command_or_subagent_output \
                   to identify each monitor):\n\n<monitor description=\"ticks\" \
                   task_id=\"t-1\">\n[1] tick-1\n[2] tick-2\n</monitor>";
    assert!(
        !tracker.handle_update(user_message(batched), &meta(), &mut sb),
        "legacy batched drain preamble still suppressed"
    );
    assert!(
        !tracker.handle_update(user_message("---"), &meta(), &mut sb),
        "legacy drain section separator still suppressed"
    );
    assert!(
        tracker.handle_update(
            user_message("what do these monitor events from my run mean (use plain words)?"),
            &meta(),
            &mut sb,
        ),
        "digit anchor: user text with both phrases but no leading count still renders"
    );
    assert_eq!(sb.len(), 3);
}
/// Helper: UserMessageChunk with `skillTokenRanges` in content-block meta.
fn user_message_with_token_ranges(text: &str, ranges: serde_json::Value) -> acp::SessionUpdate {
    let mut meta_map = acp::Meta::new();
    meta_map.insert("skillTokenRanges".into(), ranges);
    acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
        acp::TextContent::new(text.to_string()).meta(Some(meta_map)),
    )))
}
/// `skillTokenRanges` meta round-trips into a token-styled block: same
/// text, same ranges.
#[test]
fn replay_skill_token_ranges_styles_block() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        user_message_with_token_ranges(
            "great /pr-workflow all good now",
            serde_json::json!([[6, 18]]),
        ),
        &meta(),
        &mut sb,
    );
    assert_eq!(sb.len(), 1);
    match &sb.get(0).unwrap().block {
        RenderBlock::UserPrompt(block) => {
            assert_eq!(block.text, "great /pr-workflow all good now");
            assert_eq!(block.skill_token_ranges, vec![6..18]);
        }
        other => panic!("expected UserPrompt, got {:?}", other),
    }
}
/// `skillTokenRanges` index the wire text, so a `displayText` override (a
/// different coordinate space) IGNORES them — `displayAsSkill` keeps
/// owning that branch. No first-party producer stamps both.
#[test]
fn replay_display_text_ignores_skill_token_ranges() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    let mut meta_map = serde_json::Map::new();
    meta_map.insert(
        "displayText".into(),
        serde_json::Value::String("/commit now".into()),
    );
    meta_map.insert("displayAsSkill".into(), serde_json::Value::Bool(true));
    meta_map.insert("skillTokenRanges".into(), serde_json::json!([[3, 10]]));
    tracker.handle_update(
        acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new("raw wire text".to_string()).meta(Some(meta_map)),
        ))),
        &meta(),
        &mut sb,
    );
    match &sb.get(0).unwrap().block {
        RenderBlock::UserPrompt(block) => {
            assert_eq!(block.text, "/commit now", "displayText still applies");
            assert_eq!(
                block.skill_token_ranges,
                vec![0..7],
                "displayAsSkill styling (leading token), not the wire-space ranges"
            );
        }
        other => panic!("expected UserPrompt, got {:?}", other),
    }
}
/// Malformed/out-of-bounds ranges never panic; the block degrades to a
/// plain prompt (missing meta keeps the legacy fallbacks — pinned above).
#[test]
fn replay_malformed_skill_token_ranges_degrade_to_plain() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    tracker.handle_update(
        user_message_with_token_ranges("short text", serde_json::json!([[100, 200], "bogus", [3]])),
        &meta(),
        &mut sb,
    );
    match &sb.get(0).unwrap().block {
        RenderBlock::UserPrompt(block) => {
            assert_eq!(block.text, "short text");
            assert!(
                block.skill_token_ranges.is_empty(),
                "invalid ranges dropped"
            );
        }
        other => panic!("expected UserPrompt, got {:?}", other),
    }
}
#[test]
fn call_mcp_tool_coerced_to_use_tool_renders_block() {
    let tc = acp::ToolCall::new(acp::ToolCallId::new(Arc::from("mcp1")), "grafana__search")
        .kind(acp::ToolKind::Other)
        .status(acp::ToolCallStatus::Completed)
        .content(vec![])
        .raw_input(Some(serde_json::json!(
            { "variant" : "UseTool", "tool_name" : "grafana__search",
            "tool_input" : { "query" : "alerts" } }
        )))
        .locations(vec![]);
    let block = tool_call_to_block(&tc, None);
    let RenderBlock::ToolCall(ToolCallBlock::UseTool(ut)) = block else {
        panic!("expected UseTool block, got {block:?}");
    };
    assert_eq!(ut.tool_name, "grafana__search");
}
#[test]
fn call_mcp_tool_no_raw_input_does_not_panic() {
    let tc = acp::ToolCall::new(
        acp::ToolCallId::new(Arc::from("mcp2")),
        "linear__save_issue",
    )
    .kind(acp::ToolKind::Other)
    .status(acp::ToolCallStatus::Pending)
    .content(vec![])
    .locations(vec![]);
    let _block = tool_call_to_block(&tc, None);
}
#[test]
fn cursor_todo_write_suppressed_by_title() {
    assert!(is_todo_tool(&initial_tool_call("tc1", "TodoWrite")));
    assert!(is_todo_tool(&initial_tool_call("tc2", "Updating plan")));
    assert!(is_todo_tool(&initial_tool_call("tc3", "todo_write")));
}
#[test]
fn todo_write_suppressed_by_variant() {
    let mut tc = initial_tool_call("tc1", "anything");
    tc.raw_input = Some(serde_json::json!({ "variant" : "TodoWrite" }));
    assert!(is_todo_tool(&tc));
}
#[test]
fn pascal_case_todo_write_suppressed_from_scrollback() {
    let mut sb = ScrollbackState::new();
    let mut tracker = AcpUpdateTracker::new();
    for (i, title) in ["TodoWrite", "Updating plan"].iter().enumerate() {
        let id = format!("tc-todo-{i}");
        tracker.handle_update(
            tool_call(&id, acp::ToolKind::Think, title),
            &meta(),
            &mut sb,
        );
        assert_eq!(
            sb.len(),
            0,
            "todo tool with title={title:?} must be suppressed"
        );
    }
}
/// Every video ToolInput variant must route through `media_gen_block` so
/// `[Open Video]` uses the typed `MediaGenOutput.path` (not a regex scrape
/// of the JSON prompt text — fragile on Windows with %-encoded session dirs).
#[test]
fn video_tool_variants_use_typed_path_not_generic_scrape() {
    use crate::scrollback::block::BlockContent;
    let dir = tempfile::tempdir().unwrap();
    let video_path = dir.path().join("1.mp4");
    std::fs::write(&video_path, b"fake-mp4").unwrap();
    let cases: &[(&str, ToolOutput)] = &[
        (
            "ImageToVideo",
            ToolOutput::ImageToVideo(echo_agent_tools::types::output::MediaGenOutput::new(
                video_path.clone(),
            )),
        ),
        (
            "ReferenceToVideo",
            ToolOutput::ReferenceToVideo(echo_agent_tools::types::output::MediaGenOutput::new(
                video_path.clone(),
            )),
        ),
    ];
    for (variant, output) in cases {
        let tc = acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from(format!("media-{variant}"))),
            variant.to_string(),
        )
        .kind(acp::ToolKind::Other)
        .status(acp::ToolCallStatus::Completed)
        .content(vec![])
        .raw_input(Some(serde_json::json!({ "variant" : variant })))
        .raw_output(serde_json::to_value(output).ok())
        .locations(vec![]);
        let block = tool_call_to_block(&tc, None);
        let open_path = block
            .inline_open_button()
            .map(|(p, is_video)| {
                assert!(is_video, "{variant}: expected video open button");
                p
            })
            .or_else(|| block.video_references().first().map(|r| r.path.clone()))
            .unwrap_or_else(|| panic!("{variant}: missing media ref / open button"));
        assert_eq!(
            open_path, video_path,
            "{variant}: open path must be the typed MediaGenOutput.path"
        );
    }
}
#[test]
fn media_gen_ref_skips_uploaded_only_video() {
    let output =
        ToolOutput::ImageToVideo(echo_agent_tools::types::output::MediaGenOutput::uploaded(
            "https://bucket.example/videos/x.mp4".into(),
        ));
    let tc = acp::ToolCall::new(
        acp::ToolCallId::new(Arc::from("zdr-upload")),
        "image_to_video",
    )
    .kind(acp::ToolKind::Other)
    .status(acp::ToolCallStatus::Completed)
    .content(vec![])
    .raw_input(Some(serde_json::json!({ "variant": "ImageToVideo" })))
    .raw_output(serde_json::to_value(output).ok())
    .locations(vec![]);
    assert!(
        media_gen_ref(&tc).is_none(),
        "uploaded_url-only media must not claim a local open path"
    );
}
/// A tier-restricted (free / X Basic) imagine call short-circuits with the
/// EchoAgent upsell as `ToolOutput::Text` on a `Completed` status. The media
/// renderer has no file to open, so it must surface the upsell text in the
/// card body (not a bare title) and must NOT mark the card as an error.
#[test]
fn tier_restricted_media_shows_upsell_text_not_error() {
    let upsell = "Image generation is a EchoAgent feature. Upgrade at \
         https://cloud.echo-agent.invalid/echoagent?referrer=echo-agent-build";
    let output = ToolOutput::Text(echo_agent_tools::types::output::TextOutput::from(upsell));
    let tc = acp::ToolCall::new(
        acp::ToolCallId::new(Arc::from("tier-restricted-img")),
        "image_gen",
    )
    .kind(acp::ToolKind::Other)
    .status(acp::ToolCallStatus::Completed)
    .content(vec![acp::ToolCallContent::Content(acp::Content::new(
        acp::ContentBlock::Text(acp::TextContent::new(upsell)),
    ))])
    .raw_input(Some(serde_json::json!({ "variant": "ImageGen" })))
    .raw_output(serde_json::to_value(output).ok())
    .locations(vec![]);
    let RenderBlock::ToolCall(ToolCallBlock::Other(block)) = tool_call_to_block(&tc, None) else {
        panic!("expected an Other tool-call block");
    };
    assert!(
        block.is_success(),
        "the upsell is a successful result, not an error"
    );
    assert!(
        block
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("EchoAgent"),
        "upsell text must be shown in the card body, got: {:?}",
        block.output
    );
}
