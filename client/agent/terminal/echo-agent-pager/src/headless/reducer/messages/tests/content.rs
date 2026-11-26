//! Text/thinking/signature blocks and assistant-frame boundaries (default mode).

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn messages_groups_thinking_and_coalesced_text() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentThought("mulling".into()));
    r.reduce(StreamEvent::AgentMessage("Hello ".into()));
    r.reduce(StreamEvent::AgentMessage("world".into()));
    let msg = r
        .flush_assistant(Some("end_turn"))
        .expect("assistant message");
    assert_eq!(msg["type"], "assistant");
    assert_eq!(msg["message"]["stop_reason"], "end_turn");
    assert_eq!(msg["session_id"], "sess-1");
    let blocks = msg["message"]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "mulling");
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(blocks[1]["text"], "Hello world");
}

#[test]
fn messages_response_completed_stamps_assistant_frame() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentThought("plan".into()));
    r.reduce(StreamEvent::AgentMessage("hi".into()));
    assert!(
        r.reduce(StreamEvent::ResponseCompleted {
            message_id: Some("msg_real".into()),
            stop_reason: Some("end_turn".into()),
            usage: Some(ResponseUsage {
                input_tokens: 12,
                output_tokens: 7,
                cache_read_input_tokens: 3,
                cache_creation_input_tokens: 0,
                ..Default::default()
            }),
            signature: Some("sig-abc".into()),
            stop_sequence: None,
        })
        .is_empty()
    );
    let msg = r.flush_assistant(Some("stop")).expect("assistant message");
    assert_eq!(msg["message"]["id"], "msg_real");
    assert_eq!(msg["message"]["stop_reason"], "end_turn");
    assert_eq!(msg["message"]["usage"]["input_tokens"], 12);
    assert_eq!(msg["message"]["usage"]["output_tokens"], 7);
    let blocks = msg["message"]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["signature"], "sig-abc");
}

#[test]
fn messages_multiple_thinking_blocks_stamp_signature_on_last_only() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentThought("first think".into()));
    r.reduce(StreamEvent::AgentMessage("interlude".into()));
    r.reduce(StreamEvent::AgentThought("second think".into()));
    r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_a".into()),
        stop_reason: Some("end_turn".into()),
        usage: None,
        signature: Some("sig-final".into()),
        stop_sequence: None,
    });
    let msg = r
        .flush_assistant(Some("end_turn"))
        .expect("assistant message");
    let blocks = msg["message"]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "first think");
    assert_eq!(blocks[0]["signature"], "");
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(blocks[2]["type"], "thinking");
    assert_eq!(blocks[2]["thinking"], "second think");
    assert_eq!(blocks[2]["signature"], "sig-final");
}

#[test]
fn messages_response_completed_consumed_per_response() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("call".into()));
    r.reduce(response_completed("msg_a", "tool_use"));
    r.reduce(StreamEvent::ToolCallUpdate(tool_update(
        "in_progress",
        Value::Null,
    )));
    let out = r.reduce(StreamEvent::ToolCallUpdate(tool_update(
        "completed",
        json!("done"),
    )));
    let assistant = out.iter().find(|m| m["type"] == "assistant").unwrap();
    assert_eq!(assistant["message"]["id"], "msg_a");
    assert_eq!(assistant["message"]["stop_reason"], "tool_use");
    r.reduce(StreamEvent::AgentMessage("next".into()));
    let msg = r.flush_assistant(Some("end_turn")).expect("assistant");
    assert_eq!(msg["message"]["id"], "msg_0");
    assert_eq!(msg["message"]["stop_reason"], "end_turn");
}

#[test]
fn messages_signature_only_thinking_block_kept_in_frame() {
    let mut r = messages(false);
    r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-only".into()),
    });
    r.reduce(StreamEvent::AgentMessage("answer".into()));
    let msg = r
        .flush_assistant(Some("end_turn"))
        .expect("assistant frame");
    let blocks = msg["message"]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "");
    assert_eq!(blocks[0]["signature"], "sig-only");
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(blocks[1]["text"], "answer");
}

#[test]
fn messages_pure_signature_only_response_emits_thinking_block() {
    let mut r = messages(false);
    r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-only".into()),
    });
    let msg = r
        .flush_assistant(Some("end_turn"))
        .expect("assistant frame");
    let blocks = msg["message"]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "");
    assert_eq!(blocks[0]["signature"], "sig-only");
}

#[test]
fn messages_no_spurious_empty_thinking_block() {
    let mut r = messages(false);
    assert!(r.flush_assistant(Some("end_turn")).is_none());
}

#[test]
fn messages_per_response_model_reflects_mid_session_switch() {
    let mut r = messages(false);
    let mut out = Vec::new();
    out.extend(r.reduce(response_started("msg_a", Some("echo-agent-4"), 5)));
    out.extend(r.reduce(StreamEvent::AgentMessage("from A".into())));
    out.extend(r.reduce(response_completed("msg_a", "end_turn")));
    out.extend(r.reduce(response_started("msg_b", Some("echo-agent-4-fast"), 6)));
    out.extend(r.reduce(StreamEvent::AgentMessage("from B".into())));
    out.extend(r.reduce(response_completed("msg_b", "end_turn")));
    out.extend(r.finish(&end_turn()));
    let frames: Vec<&Value> = out.iter().filter(|m| m["type"] == "assistant").collect();
    assert_eq!(frames.len(), 2, "one frame per response: {out:?}");
    assert_eq!(frames[0]["message"]["id"], "msg_a");
    assert_eq!(frames[0]["message"]["model"], "echo-agent-4");
    assert_eq!(frames[1]["message"]["id"], "msg_b");
    assert_eq!(frames[1]["message"]["model"], "echo-agent-4-fast");
}

#[test]
fn messages_per_block_thinking_signatures_kept() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentThought("first think".into()));
    r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-1".into()),
    });
    r.reduce(StreamEvent::AgentMessage("interlude".into()));
    r.reduce(StreamEvent::AgentThought("second think".into()));
    r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-2".into()),
    });
    r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_a".into()),
        stop_reason: Some("end_turn".into()),
        usage: None,
        signature: Some("sig-2".into()),
        stop_sequence: None,
    });
    let msg = r
        .flush_assistant(Some("end_turn"))
        .expect("assistant message");
    let blocks = msg["message"]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "first think");
    assert_eq!(blocks[0]["signature"], "sig-1");
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(blocks[2]["type"], "thinking");
    assert_eq!(blocks[2]["thinking"], "second think");
    assert_eq!(blocks[2]["signature"], "sig-2");
}

#[test]
fn messages_assistant_frame_carries_stop_sequence() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("answer".into()));
    r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_seq".into()),
        stop_reason: Some("stop_sequence".into()),
        usage: None,
        signature: None,
        stop_sequence: Some("<END>".into()),
    });
    let msg = r
        .flush_assistant(Some("end_turn"))
        .expect("assistant frame");
    assert_eq!(msg["message"]["stop_reason"], "stop_sequence");
    assert_eq!(msg["message"]["stop_sequence"], "<END>");
}

#[test]
fn messages_consecutive_text_responses_split_into_frames() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("first".into()));
    r.reduce(response_completed("msg_a", "end_turn"));
    let out = r.reduce(StreamEvent::AgentMessage("second".into()));
    let a = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("frame A flushed on new content");
    assert_eq!(a["message"]["id"], "msg_a");
    assert_eq!(a["message"]["content"][0]["text"], "first");
    r.reduce(response_completed("msg_b", "end_turn"));
    let out2 = r.finish(&turn_end("end_turn", "second"));
    let b = out2
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("frame B flushed at finish");
    assert_eq!(b["message"]["id"], "msg_b");
    assert_eq!(b["message"]["content"][0]["text"], "second");
}

#[test]
fn messages_duplicate_response_started_does_not_merge_content() {
    let mut r = messages(false);
    let mut out = Vec::new();
    out.extend(r.reduce(response_started("msg_a", Some("echo-agent-4"), 5)));
    out.extend(r.reduce(StreamEvent::AgentMessage("A".into())));
    out.extend(r.reduce(response_started("msg_b", Some("echo-agent-4"), 6)));
    out.extend(r.reduce(StreamEvent::AgentMessage("B".into())));
    out.extend(r.reduce(response_completed("msg_b", "end_turn")));
    out.extend(r.finish(&end_turn()));
    let frames: Vec<&Value> = out.iter().filter(|m| m["type"] == "assistant").collect();
    assert_eq!(frames.len(), 2, "A flushed before B opens: {out:?}");
    assert_eq!(frames[0]["message"]["id"], "msg_a");
    assert_eq!(frames[0]["message"]["content"][0]["text"], "A");
    assert_eq!(
        frames[0]["message"]["content"].as_array().unwrap().len(),
        1,
        "A did not absorb B's content"
    );
    assert_eq!(frames[1]["message"]["id"], "msg_b");
    assert_eq!(frames[1]["message"]["content"][0]["text"], "B");
    assert_eq!(
        frames[1]["message"]["content"].as_array().unwrap().len(),
        1,
        "B did not absorb A's content"
    );
}

#[test]
fn messages_signature_only_restart_does_not_leak_signature() {
    let mut r = messages(false);
    let mut out = Vec::new();
    out.extend(r.reduce(response_started("msg_a", None, 0)));
    out.extend(r.reduce(StreamEvent::AgentThought("mull".into())));
    out.extend(r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-a".into()),
    }));
    out.extend(r.reduce(response_started("msg_b", None, 0)));
    out.extend(r.reduce(StreamEvent::AgentMessage("B".into())));
    out.extend(r.reduce(response_completed("msg_b", "end_turn")));
    out.extend(r.finish(&end_turn()));
    let frames: Vec<&Value> = out.iter().filter(|m| m["type"] == "assistant").collect();
    assert_eq!(frames.len(), 2, "{out:?}");
    assert_eq!(frames[0]["message"]["id"], "msg_a");
    assert_eq!(frames[0]["message"]["content"][0]["type"], "thinking");
    assert_eq!(frames[0]["message"]["content"][0]["signature"], "sig-a");
    assert_eq!(frames[1]["message"]["id"], "msg_b");
    assert_eq!(frames[1]["message"]["content"][0]["type"], "text");
    assert!(
        frames[1]["message"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["type"] != "thinking"),
        "no thinking block leaked into B: {:?}",
        frames[1]
    );
}

#[test]
fn messages_content_before_late_response_started_flushes_first() {
    let mut r = messages(false);
    let mut out = Vec::new();
    out.extend(r.reduce(StreamEvent::AgentMessage("early".into())));
    out.extend(r.reduce(response_started("msg_b", None, 0)));
    out.extend(r.reduce(StreamEvent::AgentMessage("late".into())));
    out.extend(r.reduce(response_completed("msg_b", "end_turn")));
    out.extend(r.finish(&end_turn()));
    let frames: Vec<&Value> = out.iter().filter(|m| m["type"] == "assistant").collect();
    assert_eq!(frames.len(), 2, "early content is its own frame: {out:?}");
    assert_eq!(frames[0]["message"]["content"][0]["text"], "early");
    assert_eq!(frames[0]["message"]["id"], "msg_0");
    assert_eq!(frames[1]["message"]["id"], "msg_b");
    assert_eq!(frames[1]["message"]["content"][0]["text"], "late");
}

#[test]
fn messages_consecutive_signature_blocks_keep_own_signature() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentThought("first".into()));
    r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-1".into()),
    });
    r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-2".into()),
    });
    let msg = r
        .flush_assistant(Some("end_turn"))
        .expect("assistant frame");
    let blocks = msg["message"]["content"].as_array().unwrap();
    assert_eq!(
        blocks.len(),
        2,
        "two thinking blocks, not collapsed: {blocks:?}"
    );
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "first");
    assert_eq!(blocks[0]["signature"], "sig-1");
    assert_eq!(blocks[1]["type"], "thinking");
    assert_eq!(blocks[1]["thinking"], "");
    assert_eq!(blocks[1]["signature"], "sig-2");
}

#[test]
fn messages_compact_completed_maps_to_system_boundary() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("hi".into()));
    let out = r.reduce(StreamEvent::Lifecycle(Lifecycle::CompactCompleted {
        pre_tokens: 1234,
    }));
    let boundary = out.last().unwrap();
    assert_eq!(boundary["type"], "system");
    assert_eq!(boundary["subtype"], "compact_boundary");
    assert_eq!(boundary["compact_metadata"]["trigger"], "auto");
    assert_eq!(boundary["compact_metadata"]["pre_tokens"], 1234);
}

#[test]
fn messages_late_response_completed_for_flushed_response_is_dropped() {
    let mut r = messages(false);
    let mut out = Vec::new();
    out.extend(r.reduce(response_started("msg_a", Some("echo-agent-4"), 1)));
    out.extend(r.reduce(StreamEvent::AgentMessage("a-text".into())));
    out.extend(r.reduce(response_started("msg_b", Some("echo-agent-4"), 2)));
    out.extend(r.reduce(StreamEvent::AgentMessage("b-text".into())));
    out.extend(r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_a".into()),
        stop_reason: Some("end_turn".into()),
        usage: Some(ResponseUsage {
            input_tokens: 99,
            output_tokens: 99,
            ..Default::default()
        }),
        signature: None,
        stop_sequence: None,
    }));
    out.extend(r.finish(&end_turn()));
    let assistants: Vec<_> = out.iter().filter(|m| m["type"] == "assistant").collect();
    assert_eq!(
        assistants.len(),
        2,
        "A flushed at B's start, B flushed at finish"
    );
    assert_eq!(assistants[0]["message"]["id"], "msg_a");
    assert_eq!(assistants[0]["message"]["content"][0]["text"], "a-text");
    let b = assistants[1];
    assert_eq!(b["message"]["id"], "msg_b");
    assert_eq!(b["message"]["content"][0]["text"], "b-text");
    assert_ne!(
        b["message"]["usage"]["input_tokens"], 99,
        "A's late usage must not land on B"
    );
}
