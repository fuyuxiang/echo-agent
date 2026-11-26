//! `--include-partial-messages` streaming framing.

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn messages_partial_deltas_emitted_when_enabled() {
    let mut r = messages(true);
    let out = r.reduce(StreamEvent::AgentMessage("hi".into()));
    let start = out
        .iter()
        .find(|m| m["event"]["type"] == "message_start")
        .expect("message_start");
    assert!(start["event"]["message"]["model"].is_string());
    assert!(start["event"]["message"]["usage"].is_object());
    let block_start = out
        .iter()
        .find(|m| m["event"]["type"] == "content_block_start")
        .expect("content_block_start");
    assert_eq!(block_start["event"]["content_block"]["type"], "text");
    assert_eq!(block_start["event"]["content_block"]["text"], "");
    let delta = stream_delta(&out);
    assert_eq!(delta["event"]["index"], 0);
    assert_eq!(delta["event"]["delta"]["type"], "text_delta");
    assert_eq!(delta["event"]["delta"]["text"], "hi");
}

#[test]
fn messages_partial_framing_closes_with_stop_reason_and_usage() {
    let mut r = messages(true);
    r.reduce(StreamEvent::AgentMessage("hi".into()));
    r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_a".into()),
        stop_reason: Some("end_turn".into()),
        usage: Some(ResponseUsage {
            input_tokens: 3,
            output_tokens: 7,
            ..Default::default()
        }),
        signature: None,
        stop_sequence: None,
    });
    let out = r.reduce(StreamEvent::AgentMessage("more".into()));
    let delta = out
        .iter()
        .find(|m| m["event"]["type"] == "message_delta")
        .expect("message_delta closes the prior message");
    assert_eq!(delta["event"]["delta"]["stop_reason"], "end_turn");
    assert_eq!(delta["event"]["usage"]["output_tokens"], 7);
    assert_eq!(delta["event"]["usage"]["input_tokens"], 3);
    assert!(out.iter().any(|m| m["event"]["type"] == "message_stop"));
}

#[test]
fn messages_partial_tool_use_framed() {
    let mut r = messages(true);
    r.reduce(StreamEvent::AgentMessage("run".into()));
    let out = r.reduce(StreamEvent::ToolCall(tool_call_ev()));
    let start = out
        .iter()
        .find(|m| {
            m["event"]["type"] == "content_block_start"
                && m["event"]["content_block"]["type"] == "tool_use"
        })
        .expect("tool_use content_block_start");
    assert_eq!(start["event"]["content_block"]["name"], "bash");
    assert!(
        out.iter()
            .any(|m| m["event"]["delta"]["type"] == "input_json_delta")
    );
}

#[test]
fn messages_partial_tool_flush_without_pending_agrees_on_stop_reason() {
    let mut r = messages(true);
    r.reduce(StreamEvent::AgentMessage("searching".into()));
    r.reduce(StreamEvent::ToolCall(tool_call_ev()));
    let out = r.reduce(StreamEvent::ToolCallUpdate(tool_update(
        "completed",
        json!("done"),
    )));
    let delta = out
        .iter()
        .find(|m| m["event"]["type"] == "message_delta")
        .expect("message_delta");
    let assistant = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("frame");
    assert_eq!(delta["event"]["delta"]["stop_reason"], "tool_use");
    assert_eq!(assistant["message"]["stop_reason"], "tool_use");
}

#[test]
fn messages_partial_delta_index_tracks_block() {
    let mut r = messages(true);
    let t = r.reduce(StreamEvent::AgentThought("mull".into()));
    assert_eq!(stream_delta(&t)["event"]["index"], 0);
    let x = r.reduce(StreamEvent::AgentMessage("hi".into()));
    assert_eq!(stream_delta(&x)["event"]["index"], 1);
    assert!(x.iter().any(|m| m["event"]["type"] == "content_block_stop"));
}

#[test]
fn messages_partial_thinking_then_text_defers_signature_to_frame() {
    let mut r = messages(true);
    let mut out = Vec::new();
    out.extend(r.reduce(StreamEvent::AgentThought("mull".into())));
    out.extend(r.reduce(StreamEvent::AgentMessage("hi".into())));
    out.extend(r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_real".into()),
        stop_reason: Some("end_turn".into()),
        usage: None,
        signature: Some("sig-xyz".into()),
        stop_sequence: None,
    }));
    assert!(
        !out.iter()
            .any(|m| m["event"]["delta"]["type"] == "signature_delta")
    );
    let start = out
        .iter()
        .find(|m| m["event"]["type"] == "message_start")
        .expect("message_start");
    assert_eq!(start["event"]["message"]["id"], "msg_0");
    let frame = r
        .flush_assistant(Some("end_turn"))
        .expect("assistant frame");
    assert_eq!(frame["message"]["id"], "msg_real");
    assert_eq!(frame["message"]["content"][0]["signature"], "sig-xyz");
}

#[test]
fn messages_partial_response_started_emits_real_id_and_input_usage() {
    let mut r = messages(true);
    let mut out = Vec::new();
    out.extend(r.reduce(StreamEvent::ResponseStarted {
        message_id: Some("msg_real".into()),
        model: Some("echo-agent-4".into()),
        input_tokens: 42,
        cache_read_input_tokens: 100,
        cache_creation_input_tokens: 20,
    }));
    out.extend(r.reduce(StreamEvent::AgentThought("mull".into())));
    out.extend(r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-xyz".into()),
    }));
    out.extend(r.reduce(StreamEvent::AgentMessage("hi".into())));
    out.extend(r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_real".into()),
        stop_reason: Some("end_turn".into()),
        usage: None,
        signature: Some("sig-xyz".into()),
        stop_sequence: None,
    }));

    let start = out
        .iter()
        .find(|m| m["event"]["type"] == "message_start")
        .expect("message_start");
    assert_eq!(start["event"]["message"]["id"], "msg_real");
    assert_eq!(start["event"]["message"]["usage"]["input_tokens"], 42);
    assert_eq!(
        start["event"]["message"]["usage"]["cache_read_input_tokens"],
        100
    );
    assert_eq!(
        start["event"]["message"]["usage"]["cache_creation_input_tokens"],
        20
    );
    assert_eq!(start["event"]["message"]["usage"]["output_tokens"], 0);

    let sig = out
        .iter()
        .position(|m| m["event"]["delta"]["type"] == "signature_delta")
        .expect("signature_delta emitted in order");
    assert_eq!(out[sig]["event"]["delta"]["signature"], "sig-xyz");
    let stop = out
        .iter()
        .position(|m| m["event"]["type"] == "content_block_stop")
        .expect("content_block_stop");
    assert!(sig < stop, "signature_delta precedes content_block_stop");

    let frame = r
        .flush_assistant(Some("end_turn"))
        .expect("assistant frame");
    assert_eq!(frame["message"]["id"], "msg_real");
    assert_eq!(frame["message"]["content"][0]["signature"], "sig-xyz");
}

#[test]
fn messages_partial_response_started_ids_do_not_leak_across_responses() {
    let mut r = messages(true);
    let mut out = Vec::new();
    out.extend(r.reduce(StreamEvent::ResponseStarted {
        message_id: Some("msg_real".into()),
        model: None,
        input_tokens: 9,
        cache_read_input_tokens: 5,
        cache_creation_input_tokens: 0,
    }));
    out.extend(r.reduce(StreamEvent::AgentMessage("one".into())));
    out.extend(r.reduce(response_completed("msg_real", "end_turn")));
    out.extend(r.reduce(StreamEvent::AgentMessage("two".into())));
    let starts: Vec<&Value> = out
        .iter()
        .filter(|m| m["event"]["type"] == "message_start")
        .collect();
    assert_eq!(starts[0]["event"]["message"]["id"], "msg_real");
    assert_eq!(starts[0]["event"]["message"]["usage"]["input_tokens"], 9);
    assert_eq!(
        starts[0]["event"]["message"]["usage"]["cache_read_input_tokens"],
        5
    );
    assert_eq!(starts[1]["event"]["message"]["id"], "msg_0");
    assert_eq!(starts[1]["event"]["message"]["usage"]["input_tokens"], 0);
    assert_eq!(
        starts[1]["event"]["message"]["usage"]["cache_read_input_tokens"],
        0
    );
    assert_eq!(
        starts[1]["event"]["message"]["usage"]["cache_creation_input_tokens"],
        0
    );
}

#[test]
fn messages_partial_thinking_terminal_emits_signature_delta() {
    let mut r = messages(true);
    r.reduce(StreamEvent::AgentThought("mull".into()));
    r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_a".into()),
        stop_reason: Some("end_turn".into()),
        usage: None,
        signature: Some("sig-term".into()),
        stop_sequence: None,
    });
    let out = r.reduce(StreamEvent::AgentMessage("answer".into()));
    let sig = out
        .iter()
        .position(|m| m["event"]["delta"]["type"] == "signature_delta")
        .expect("signature_delta emitted");
    assert_eq!(out[sig]["event"]["delta"]["signature"], "sig-term");
    let stop = out
        .iter()
        .position(|m| m["event"]["type"] == "content_block_stop")
        .expect("content_block_stop");
    assert!(sig < stop, "signature_delta precedes content_block_stop");
}

#[test]
fn messages_partial_message_start_ids_are_unique() {
    let mut r = messages(true);
    let mut out = Vec::new();
    out.extend(r.reduce(StreamEvent::AgentMessage("one".into())));
    out.extend(r.reduce(response_completed("msg_a", "end_turn")));
    out.extend(r.reduce(StreamEvent::AgentMessage("two".into())));
    let ids: Vec<String> = out
        .iter()
        .filter(|m| m["event"]["type"] == "message_start")
        .map(|m| m["event"]["message"]["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["msg_0", "msg_1"]);
}

#[test]
fn messages_partial_signature_only_thinking_block_emits_framing() {
    let mut r = messages(true);
    let mut out = Vec::new();
    out.extend(r.reduce(response_started("msg_a", Some("echo-agent-4"), 5)));
    out.extend(r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-only".into()),
    }));
    out.extend(r.reduce(StreamEvent::AgentMessage("answer".into())));
    out.extend(r.reduce(response_completed("msg_a", "end_turn")));
    out.extend(r.finish(&end_turn()));
    let cb_start = out
        .iter()
        .position(|m| {
            m["event"]["type"] == "content_block_start"
                && m["event"]["content_block"]["type"] == "thinking"
        })
        .expect("thinking content_block_start");
    let sig = out
        .iter()
        .position(|m| m["event"]["delta"]["type"] == "signature_delta")
        .expect("signature_delta");
    assert_eq!(out[cb_start]["event"]["index"], 0);
    assert_eq!(out[sig]["event"]["delta"]["signature"], "sig-only");
    assert!(
        cb_start < sig,
        "content_block_start precedes signature_delta"
    );
    let frame = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("frame");
    let blocks = frame["message"]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["signature"], "sig-only");
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(blocks[1]["text"], "answer");
}

#[test]
fn messages_partial_per_block_signature_deltas() {
    let mut r = messages(true);
    let mut out = Vec::new();
    out.extend(r.reduce(StreamEvent::AgentThought("first think".into())));
    out.extend(r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-1".into()),
    }));
    out.extend(r.reduce(StreamEvent::AgentMessage("interlude".into())));
    out.extend(r.reduce(StreamEvent::AgentThought("second think".into())));
    out.extend(r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-2".into()),
    }));
    out.extend(r.finish(&end_turn()));
    let sigs: Vec<&str> = out
        .iter()
        .filter(|m| m["event"]["delta"]["type"] == "signature_delta")
        .map(|m| m["event"]["delta"]["signature"].as_str().unwrap())
        .collect();
    assert_eq!(sigs, vec!["sig-1", "sig-2"], "each block keeps its own sig");
}

#[test]
fn messages_partial_empty_response_still_frames_message() {
    let mut r = messages(true);
    r.reduce(response_started("msg_empty", Some("echo-agent-4"), 5));
    r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_empty".into()),
        stop_reason: Some("end_turn".into()),
        usage: Some(ResponseUsage {
            input_tokens: 5,
            output_tokens: 0,
            ..Default::default()
        }),
        signature: None,
        stop_sequence: None,
    });
    let out = r.finish(&end_turn());
    let start = out
        .iter()
        .find(|m| m["event"]["type"] == "message_start")
        .expect("message_start for the empty response");
    assert!(out.iter().any(|m| m["event"]["type"] == "message_delta"));
    assert!(out.iter().any(|m| m["event"]["type"] == "message_stop"));
    assert!(
        !out.iter().any(|m| m["event"]["type"]
            .as_str()
            .is_some_and(|t| t.starts_with("content_block"))),
        "no content_block_* events: {out:?}"
    );
    assert!(out.iter().all(|m| m["type"] != "assistant"), "{out:?}");
    assert_eq!(start["event"]["message"]["id"], "msg_empty");
    assert_eq!(start["event"]["message"]["usage"]["input_tokens"], 5);
}

#[test]
fn messages_partial_empty_then_real_response_do_not_cross_attribute() {
    let mut r = messages(true);
    let mut out = Vec::new();
    out.extend(r.reduce(response_started("msg_a", None, 11)));
    out.extend(r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_a".into()),
        stop_reason: Some("end_turn".into()),
        usage: Some(ResponseUsage {
            input_tokens: 11,
            output_tokens: 0,
            ..Default::default()
        }),
        signature: None,
        stop_sequence: None,
    }));
    out.extend(r.reduce(response_started("msg_b", None, 22)));
    out.extend(r.reduce(StreamEvent::AgentMessage("real".into())));
    let starts: Vec<&Value> = out
        .iter()
        .filter(|m| m["event"]["type"] == "message_start")
        .collect();
    assert_eq!(starts.len(), 2, "one envelope per response: {out:?}");
    assert_eq!(starts[0]["event"]["message"]["id"], "msg_a");
    assert_eq!(starts[0]["event"]["message"]["usage"]["input_tokens"], 11);
    assert_eq!(starts[1]["event"]["message"]["id"], "msg_b");
    assert_eq!(starts[1]["event"]["message"]["usage"]["input_tokens"], 22);
}

#[test]
fn messages_partial_message_delta_carries_stop_sequence() {
    let mut r = messages(true);
    r.reduce(StreamEvent::AgentMessage("answer".into()));
    r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_seq".into()),
        stop_reason: Some("stop_sequence".into()),
        usage: None,
        signature: None,
        stop_sequence: Some("<END>".into()),
    });
    let out = r.reduce(StreamEvent::AgentMessage("more".into()));
    let delta = out
        .iter()
        .find(|m| m["event"]["type"] == "message_delta")
        .expect("message_delta closes the prior message");
    assert_eq!(delta["event"]["delta"]["stop_reason"], "stop_sequence");
    assert_eq!(delta["event"]["delta"]["stop_sequence"], "<END>");
    let assistant = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("assistant frame");
    assert_eq!(assistant["message"]["stop_sequence"], "<END>");
    let start = out.iter().find(|m| m["event"]["type"] == "message_start");
    if let Some(start) = start {
        assert!(start["event"]["message"]["stop_sequence"].is_null());
    }
}

#[test]
fn messages_partial_consecutive_signature_blocks_keep_own_signature() {
    let mut r = messages(true);
    let mut out = Vec::new();
    out.extend(r.reduce(StreamEvent::AgentThought("first".into())));
    out.extend(r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-1".into()),
    }));
    out.extend(r.reduce(StreamEvent::ReasoningCompleted {
        signature: Some("sig-2".into()),
    }));
    out.extend(r.finish(&end_turn()));
    let sigs: Vec<&str> = out
        .iter()
        .filter(|m| m["event"]["delta"]["type"] == "signature_delta")
        .map(|m| m["event"]["delta"]["signature"].as_str().unwrap())
        .collect();
    assert_eq!(sigs, vec!["sig-1", "sig-2"], "{out:?}");
    let frame = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("assistant frame");
    let blocks = frame["message"]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    assert_eq!(blocks[0]["signature"], "sig-1");
    assert_eq!(blocks[1]["signature"], "sig-2");
}
