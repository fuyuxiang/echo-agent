//! Client `tool_use`/`tool_result` ordering (non-web).

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn messages_tool_use_grouped_then_user_results() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("running it".into()));
    assert!(
        r.reduce(StreamEvent::ToolCallUpdate(tool_update(
            "in_progress",
            Value::Null
        )))
        .is_empty()
    );
    let out = r.reduce(StreamEvent::ToolCallUpdate(tool_update(
        "completed",
        json!("done"),
    )));
    let assistant = out.iter().find(|m| m["type"] == "assistant").unwrap();
    assert_eq!(assistant["message"]["stop_reason"], "tool_use");
    assert!(out.iter().all(|m| m["type"] != "user"), "result is grouped");
    let fin = r.finish(&end_turn());
    let user = fin.iter().find(|m| m["type"] == "user").unwrap();
    assert_eq!(user["message"]["content"][0]["tool_use_id"], "t1");
    assert_eq!(user["message"]["content"][0]["is_error"], false);
    assert_eq!(user["message"]["content"][0]["content"], "done");
}

#[test]
fn messages_sequential_tool_rounds_interleave_without_response_started() {
    let mut r = messages(false);
    let mut out = Vec::new();
    out.extend(r.reduce(StreamEvent::ToolCall(tool_call_ev())));
    out.extend(r.reduce(StreamEvent::ToolCallUpdate(tool_update(
        "completed",
        json!("a"),
    ))));
    let mut second = tool_call_ev();
    second.tool_call_id = "t2".into();
    out.extend(r.reduce(StreamEvent::ToolCall(second)));
    let mut u2 = tool_update("completed", json!("b"));
    u2.tool_call_id = "t2".into();
    out.extend(r.reduce(StreamEvent::ToolCallUpdate(u2)));
    out.extend(r.finish(&end_turn()));
    let seq: Vec<&str> = out
        .iter()
        .filter_map(|m| m["type"].as_str())
        .filter(|t| *t == "assistant" || *t == "user")
        .collect();
    assert_eq!(
        seq,
        ["assistant", "user", "assistant", "user"],
        "each tool round interleaves assistant -> user before the next round"
    );
    let users: Vec<_> = out.iter().filter(|m| m["type"] == "user").collect();
    assert_eq!(users[0]["message"]["content"][0]["tool_use_id"], "t1");
    assert_eq!(users[1]["message"]["content"][0]["tool_use_id"], "t2");
    let assts: Vec<_> = out.iter().filter(|m| m["type"] == "assistant").collect();
    assert_eq!(assts[0]["message"]["content"][0]["id"], "t1");
    assert_eq!(assts[1]["message"]["content"][0]["id"], "t2");
}

#[test]
fn messages_text_then_tool_led_response_split_into_frames() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("first".into()));
    r.reduce(response_completed("msg_a", "pause_turn"));
    let out = r.reduce(response_completed("msg_b", "tool_use"));
    let a = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("frame A flushed at B's boundary");
    assert_eq!(a["message"]["id"], "msg_a");
    assert_eq!(a["message"]["content"][0]["text"], "first");
    r.reduce(StreamEvent::ToolCall(tool_call_ev()));
    let out2 = r.reduce(StreamEvent::ToolCallUpdate(tool_update(
        "completed",
        json!("done"),
    )));
    let b = out2
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("frame B");
    assert_eq!(b["message"]["id"], "msg_b");
    assert_eq!(b["message"]["content"][0]["type"], "tool_use");
}

#[test]
fn messages_result_reflects_final_text_not_earlier_response() {
    let mut r = messages(false);
    let mut out = Vec::new();
    out.extend(r.reduce(StreamEvent::AgentMessage("hi".into())));
    out.extend(r.reduce(response_completed("msg_a", "end_turn")));
    out.extend(r.reduce(StreamEvent::AgentThought("planning".into())));
    out.extend(r.reduce(StreamEvent::ToolCall(tool_call_ev())));
    out.extend(r.reduce(StreamEvent::ToolCallUpdate(tool_update(
        "completed",
        json!("done"),
    ))));
    out.extend(r.finish(&turn_end("end_turn", "hi")));
    let result = out.last().expect("result line");
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["result"], "", "{result:?}");
    let frames: Vec<&Value> = out.iter().filter(|m| m["type"] == "assistant").collect();
    assert_eq!(frames.len(), 2, "{out:?}");
    assert_eq!(frames[0]["message"]["content"][0]["text"], "hi");
    assert!(
        frames[1]["message"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["type"] != "text"),
        "final frame is text-less: {:?}",
        frames[1]
    );
}

#[test]
fn messages_unmatched_client_tool_use_reconciled_at_finish() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("running it".into()));
    r.reduce(StreamEvent::ToolCall(tool_call_ev()));
    let out = r.finish(&end_turn());
    let assistant = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("assistant frame");
    assert!(
        assistant["message"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["type"] == "tool_use" && b["id"] == "t1"),
        "tool_use present: {assistant:?}"
    );
    let user = out
        .iter()
        .find(|m| m["type"] == "user")
        .expect("reconciled tool_result");
    let block = &user["message"]["content"][0];
    assert_eq!(block["type"], "tool_result");
    assert_eq!(block["tool_use_id"], "t1");
    assert_eq!(block["is_error"], true, "{block:?}");
    assert_eq!(block["content"], "tool call did not complete");
}

#[test]
fn messages_parallel_tool_results_ordered_by_tool_use_not_completion() {
    let mut r = messages(false);
    r.reduce(StreamEvent::ToolCall(tool_call_ev()));
    let mut b = tool_call_ev();
    b.tool_call_id = "t2".into();
    r.reduce(StreamEvent::ToolCall(b));
    let mut ub = tool_update("completed", json!("b-result"));
    ub.tool_call_id = "t2".into();
    r.reduce(StreamEvent::ToolCallUpdate(ub));
    r.reduce(StreamEvent::ToolCallUpdate(tool_update(
        "completed",
        json!("a-result"),
    )));
    let fin = r.finish(&end_turn());
    let user = fin
        .iter()
        .find(|m| m["type"] == "user")
        .expect("grouped user message");
    let content = user["message"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["tool_use_id"], "t1");
    assert_eq!(content[0]["content"], "a-result");
    assert_eq!(content[1]["tool_use_id"], "t2");
    assert_eq!(content[1]["content"], "b-result");
}
