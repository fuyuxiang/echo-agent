//! Backend web search folding and reconciliation.

use super::*;
use pretty_assertions::assert_eq;
// `acp` binds to the protocol crate here to avoid resolving to the sibling module through `use super::*`.
use agent_client_protocol as acp;

#[test]
fn tool_call_event_classifies_only_backend_web_search() {
    let ws = acp::ToolCall::new(
        acp::ToolCallId::from("ws1".to_string()),
        "Web search:".to_string(),
    )
    .kind(acp::ToolKind::Search)
    .status(acp::ToolCallStatus::InProgress)
    .raw_input(Some(json!({"variant": "WebSearch", "backend": true})))
    .meta(json!({"backend": true}).as_object().cloned());
    assert!(tool_call_event(&ws).backend_web_search);

    let xs = acp::ToolCall::new(
        acp::ToolCallId::from("xs1".to_string()),
        "X search:".to_string(),
    )
    .raw_input(Some(json!({"variant": "XSearch", "backend": true})))
    .meta(json!({"backend": true}).as_object().cloned());
    assert!(!tool_call_event(&xs).backend_web_search);

    let client = acp::ToolCall::new(acp::ToolCallId::from("c1".to_string()), "bash".to_string())
        .raw_input(Some(json!({"variant": "WebSearch"})));
    assert!(!tool_call_event(&client).backend_web_search);
}

#[test]
fn tool_name_and_kind_prefer_canonical_x_ai_tool_over_acp_fields() {
    let named = acp::ToolCall::new(
        acp::ToolCallId::from("t1".to_string()),
        "X search:".to_string(),
    )
    .kind(acp::ToolKind::Other)
    .meta(
        json!({"echo.agent/tool": {"name": "x_search", "kind": "search"}})
            .as_object()
            .cloned(),
    );
    let ev = tool_call_event(&named);
    assert_eq!(ev.tool_name, "x_search");
    assert_eq!(ev.tool_kind.as_deref(), Some("search"));

    let bare = acp::ToolCall::new(acp::ToolCallId::from("t2".to_string()), "Read".to_string())
        .kind(acp::ToolKind::Read);
    let ev = tool_call_event(&bare);
    assert_eq!(ev.tool_name, "Read");
    assert_eq!(ev.tool_kind.as_deref(), Some("read"));
}

#[test]
fn messages_backend_web_search_inline_single_frame() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("Let me search. ".into()));
    assert!(
        r.reduce(StreamEvent::ToolCall(web_search_call("ws1")))
            .is_empty(),
        "backend web search ToolCall emits no client tool_use"
    );
    assert!(
        r.reduce(StreamEvent::ToolCallUpdate(web_search_done("ws1")))
            .is_empty(),
        "completion folds inline, does not flush a frame or a user result"
    );
    r.reduce(StreamEvent::AgentMessage("Found it.".into()));
    r.reduce(response_completed("msg_real", "end_turn"));
    let out = r.finish(&end_turn());
    let assistants: Vec<_> = out.iter().filter(|m| m["type"] == "assistant").collect();
    assert_eq!(
        assistants.len(),
        1,
        "web search stays in one assistant frame"
    );
    assert!(
        out.iter().all(|m| m["type"] != "user"),
        "backend web search is not a client user tool_result"
    );
    let content = assistants[0]["message"]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "Let me search. ");
    assert_eq!(content[1]["type"], "server_tool_use");
    assert_eq!(content[1]["name"], "web_search");
    assert_eq!(content[1]["id"], "ws1");
    assert_eq!(content[1]["input"]["query"], "rust async runtime");
    assert_eq!(content[2]["type"], "web_search_tool_result");
    assert_eq!(content[2]["tool_use_id"], "ws1");
    assert_eq!(content[2]["content"][0]["type"], "web_search_result");
    assert_eq!(content[2]["content"][0]["url"], "https://tokio.rs");
    assert_eq!(content[2]["content"][0]["title"], "Tokio");
    assert_eq!(content[2]["content"][1]["url"], "https://async.rs");
    assert_eq!(content[2]["content"][1]["title"], "https://async.rs");
    assert_eq!(content[3]["type"], "text");
    assert_eq!(content[3]["text"], "Found it.");
}

#[test]
fn messages_backend_web_search_inline_partial() {
    let mut r = messages(true);
    let mut out = Vec::new();
    out.extend(r.reduce(StreamEvent::AgentMessage("Let me search. ".into())));
    out.extend(r.reduce(StreamEvent::ToolCall(web_search_call("ws1"))));
    out.extend(r.reduce(StreamEvent::ToolCallUpdate(web_search_done("ws1"))));
    out.extend(r.reduce(StreamEvent::AgentMessage("Found it.".into())));

    let stu_start = out
        .iter()
        .find(|m| {
            m["event"]["type"] == "content_block_start"
                && m["event"]["content_block"]["type"] == "server_tool_use"
        })
        .expect("server_tool_use content_block_start");
    assert_eq!(stu_start["event"]["index"], 1);
    assert_eq!(stu_start["event"]["content_block"]["name"], "web_search");
    assert_eq!(stu_start["event"]["content_block"]["id"], "ws1");
    let ijd = out
        .iter()
        .find(|m| m["event"]["delta"]["type"] == "input_json_delta")
        .expect("input_json_delta carrying the query");
    assert!(
        ijd["event"]["delta"]["partial_json"]
            .as_str()
            .unwrap()
            .contains("rust async runtime")
    );

    let res_start = out
        .iter()
        .find(|m| {
            m["event"]["type"] == "content_block_start"
                && m["event"]["content_block"]["type"] == "web_search_tool_result"
        })
        .expect("web_search_tool_result content_block_start");
    assert_eq!(res_start["event"]["index"], 2);
    assert_eq!(res_start["event"]["content_block"]["tool_use_id"], "ws1");
    assert_eq!(
        res_start["event"]["content_block"]["content"][0]["url"],
        "https://tokio.rs"
    );

    let text_delta = out
        .iter()
        .rev()
        .find(|m| m["event"]["delta"]["type"] == "text_delta")
        .expect("trailing text_delta");
    assert_eq!(text_delta["event"]["index"], 3);

    let fin = r.finish(&end_turn());
    assert!(fin.iter().all(|m| m["type"] != "user"));
    let frame = fin
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("assistant frame");
    let content = frame["message"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 4);
    assert_eq!(content[1]["type"], "server_tool_use");
    assert_eq!(content[2]["type"], "web_search_tool_result");
}

#[test]
fn messages_backend_web_search_failed_emits_error_not_counted() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("searching".into()));
    r.reduce(StreamEvent::ToolCall(web_search_call("ws1")));
    assert!(
        r.reduce(StreamEvent::ToolCallUpdate(web_search_failed("ws1")))
            .is_empty()
    );
    let out = r.finish(&end_turn());
    let assistant = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("assistant frame");
    let content = assistant["message"]["content"].as_array().unwrap();
    let stu = content
        .iter()
        .find(|b| b["type"] == "server_tool_use")
        .expect("server_tool_use still paired with the error result");
    assert_eq!(stu["id"], "ws1");
    let res = content
        .iter()
        .find(|b| b["type"] == "web_search_tool_result")
        .expect("web_search_tool_result");
    assert_eq!(res["tool_use_id"], "ws1");
    assert_eq!(res["content"]["type"], "web_search_tool_result_error");
    assert_eq!(res["content"]["error_code"], "unavailable");
    assert!(out.iter().all(|m| m["type"] != "user"));
    let result = out.last().unwrap();
    assert_eq!(result["usage"]["server_tool_use"]["web_search_requests"], 0);
}

#[test]
fn messages_backend_web_search_non_search_action_uses_generic_split() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("opening a page".into()));
    r.reduce(StreamEvent::ToolCall(web_search_call("ws1")));
    let out = r.reduce(StreamEvent::ToolCallUpdate(web_search_non_search("ws1")));
    let assistant = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("assistant frame");
    let content = assistant["message"]["content"].as_array().unwrap();
    assert!(
        content
            .iter()
            .all(|b| b["type"] != "server_tool_use" && b["type"] != "web_search_tool_result"),
        "no fabricated web-search blocks: {content:?}"
    );
    let tu = content
        .iter()
        .find(|b| b["type"] == "tool_use")
        .expect("generic client tool_use");
    assert_eq!(tu["id"], "ws1");
    assert_eq!(tu["name"], "web_search");
    let fin = r.finish(&end_turn());
    let user = fin
        .iter()
        .find(|m| m["type"] == "user")
        .expect("generic user tool_result");
    assert_eq!(user["message"]["content"][0]["tool_use_id"], "ws1");
    let result = fin.last().unwrap();
    assert_eq!(result["usage"]["server_tool_use"]["web_search_requests"], 0);
}

#[test]
fn messages_unresolved_backend_web_search_flushed_at_turn_end() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("searching".into()));
    r.reduce(StreamEvent::ToolCall(web_search_call("ws1")));
    let out = r.finish(&end_turn());
    let assistant = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("assistant frame carries the reconciled search");
    let content = assistant["message"]["content"].as_array().unwrap();
    let stu = content
        .iter()
        .find(|b| b["type"] == "server_tool_use")
        .expect("server_tool_use for the observed invocation");
    assert_eq!(stu["id"], "ws1");
    let res = content
        .iter()
        .find(|b| b["type"] == "web_search_tool_result")
        .expect("paired result");
    assert_eq!(res["tool_use_id"], "ws1");
    assert_eq!(res["content"]["type"], "web_search_tool_result_error");
    assert_eq!(res["content"]["error_code"], "unavailable");
    assert!(out.iter().all(|m| m["type"] != "user"));
    let result = out.last().unwrap();
    assert_eq!(result["usage"]["server_tool_use"]["web_search_requests"], 0);
}

#[test]
fn messages_unresolved_backend_web_search_flushed_partial() {
    let mut r = messages(true);
    r.reduce(StreamEvent::AgentMessage("searching".into()));
    r.reduce(StreamEvent::ToolCall(web_search_call("ws1")));
    let out = r.finish(&end_turn());
    assert!(
        out.iter()
            .any(|m| m["event"]["type"] == "content_block_start"
                && m["event"]["content_block"]["type"] == "server_tool_use"),
        "partial server_tool_use framed: {out:?}"
    );
    let assistant = out
        .iter()
        .find(|m| m["type"] == "assistant")
        .expect("assistant frame");
    let content = assistant["message"]["content"].as_array().unwrap();
    assert!(
        content.iter().any(|b| b["type"] == "web_search_tool_result"
            && b["content"]["type"] == "web_search_tool_result_error"),
        "error result in frame: {content:?}"
    );
}

#[test]
fn messages_backend_web_searches_ordered_by_invocation_not_id() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("searching".into()));
    r.reduce(StreamEvent::ToolCall(web_search_call("zzz")));
    r.reduce(StreamEvent::ToolCall(tool_call_ev()));
    r.reduce(StreamEvent::ToolCall(web_search_call("aaa")));
    r.reduce(StreamEvent::ToolCallUpdate(tool_update(
        "completed",
        json!("done"),
    )));
    let out = r.finish(&end_turn());
    let ids: Vec<String> = out
        .iter()
        .filter(|m| m["type"] == "assistant")
        .flat_map(|m| m["message"]["content"].as_array().unwrap().clone())
        .filter(|b| b["type"] == "server_tool_use")
        .map(|b| b["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["zzz", "aaa"],
        "backend searches emit in invocation order, not id-lexicographic order: {out:?}"
    );
}

#[test]
fn messages_result_counts_web_search_requests() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AgentMessage("searching".into()));
    r.reduce(StreamEvent::ToolCall(web_search_call("ws1")));
    r.reduce(StreamEvent::ToolCallUpdate(web_search_done("ws1")));
    r.reduce(StreamEvent::ToolCall(web_search_call("ws2")));
    r.reduce(StreamEvent::ToolCallUpdate(web_search_done("ws2")));
    let out = r.finish(&end_turn());
    let result = out.last().expect("result line");
    assert_eq!(result["usage"]["server_tool_use"]["web_search_requests"], 2);
    assert!(result["usage"]["server_tool_use"]["web_fetch_requests"].is_null());
}
