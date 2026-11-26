//! Fixtures shared by the conversation tests.

use super::*;

pub(super) fn count_cache_control(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(map) => {
            usize::from(map.contains_key("cache_control"))
                + map.values().map(count_cache_control).sum::<usize>()
        }
        serde_json::Value::Array(items) => items.iter().map(count_cache_control).sum(),
        _ => 0,
    }
}

pub(super) fn marker_on_last_block(message: &serde_json::Value) -> Option<&str> {
    message
        .get("content")?
        .as_array()?
        .last()?
        .pointer("/cache_control/type")?
        .as_str()
}

pub(super) fn agent_turn(n: usize) -> Vec<ConversationItem> {
    let id = format!("call_{n}");
    vec![
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: id.as_str().into(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "src/main.rs"}"#.into(),
        }]),
        ConversationItem::tool_result(id, "fn main() {}"),
    ]
}

pub(super) fn agent_request(turns: usize) -> serde_json::Value {
    let mut items = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Fix the bug"),
    ];
    for n in 0..turns {
        items.extend(agent_turn(n));
    }
    serde_json::to_value(build_messages_request(
        &ConversationRequest::from_items(items).with_model("messages-compatible-model"),
    ))
    .unwrap()
}

pub(super) fn btw_prepare_items(mut items: Vec<ConversationItem>) -> Vec<ConversationItem> {
    // Strip reasoning (same as strip_reasoning_blocks): filter out
    // sibling Reasoning items entirely.
    items.retain(|item| !matches!(item, ConversationItem::Reasoning(_)));
    // Truncate trailing incomplete tool runs.
    while let Some(last) = items.last() {
        match last {
            ConversationItem::Assistant(a) if !a.tool_calls.is_empty() => {
                items.pop();
            }
            ConversationItem::ToolResult(_) => {
                items.pop();
            }
            _ => break,
        }
    }
    items.push(ConversationItem::user("btw what is X?"));
    items
}

pub(super) fn btw_mid_turn_conversation() -> Vec<ConversationItem> {
    vec![
        ConversationItem::system("You are helpful."),
        ConversationItem::user("Fix the bug"),
        // Completed turn with thinking
        ConversationItem::Assistant(AssistantItem {
            content: "I'll look at the code.".into(),
            tool_calls: vec![],
            model_id: Some("messages-compatible-model".into()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        // Completed tool pair
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: r#"{"path":"src/main.rs"}"#.into(),
            }],
            model_id: Some("messages-compatible-model".into()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("call_1", "fn main() {}"),
        ConversationItem::Assistant(AssistantItem {
            content: "I see the issue.".into(),
            tool_calls: vec![],
            model_id: Some("messages-compatible-model".into()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        // Mid-turn: orphaned tool_use (no result yet)
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "call_2".into(),
                name: "search_replace".to_string(),
                arguments: "{}".into(),
            }],
            model_id: Some("messages-compatible-model".into()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ]
}

pub(super) fn assistant_with_calls(calls: &[(&str, &str)]) -> ConversationItem {
    ConversationItem::Assistant(AssistantItem {
        content: String::new().into(),
        tool_calls: calls
            .iter()
            .map(|(id, name)| ToolCall {
                id: (*id).into(),
                name: (*name).into(),
                arguments: "{}".into(),
            })
            .collect(),
        model_id: None,
        model_fingerprint: None,
        reasoning_effort: None,
    })
}

pub(super) fn make_response(message: ConversationItem) -> ConversationResponse {
    ConversationResponse {
        items: vec![message],
        stop_reason: Some(StopReason::Stop),
        usage: None,
        cost_usd_ticks: None,
        message_chunks_emitted: 0,
        doom_loop_signals: Vec::new(),
        stop_message: None,
        message_id: None,
        raw_stop_reason: None,
        stop_sequence: None,
    }
}

// KV Cache Invariant Tests (adapted to sibling-Reasoning)
//
// These tests enforce prefix stability and correct turn ordering for the
// Responses API input construction. Prompt caching (server-side prefix
// match) requires that request N's serialised input is a strict prefix
// of request N+1's. Any re-ordering of items -- especially reasoning
// items -- destroys the prefix and tanks the cache hit rate.
//
// The invariant asserted is `&input2[..input1.len()] == input1` for
// every pair of consecutive turns.
//
// In the sibling-Reasoning refactor, reasoning rides as
// `ConversationItem::Reasoning(rs::ReasoningItem)` siblings in the flat
// ordered `items` list. The serialized wire shape is produced by the
// `From<&ConversationRequest> for rs::CreateResponse` impl with no
// placeholder/splice dance. The old `__RAW_OUTPUT_PLACEHOLDER_`
// / `extract_raw_input_items` / `splice_raw_input_items` tests are
// structurally obsolete and are not ported; the invariants they pinned
// are preserved here in a backend-shape-agnostic form.

pub(super) fn reasoning_sibling(
    id: &str,
    summary_text: &str,
    encrypted: Option<&str>,
) -> ConversationItem {
    ConversationItem::Reasoning(rs::ReasoningItem {
        id: id.to_string(),
        summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
            text: summary_text.to_string(),
        })],
        content: None,
        encrypted_content: encrypted.map(str::to_owned),
        status: None,
    })
}

pub(super) fn input_items_json(req: &ConversationRequest) -> Vec<serde_json::Value> {
    let cr: rs::CreateResponse = req.into();
    let mut body = serde_json::to_value(&cr).unwrap();
    patch_reasoning_text_types(&mut body);
    body["input"].as_array().cloned().unwrap_or_default()
}

pub(super) fn summarise_input(items: &[serde_json::Value]) -> Vec<String> {
    items
        .iter()
        .map(|v| {
            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("?");
            if let Some(role) = v.get("role").and_then(|r| r.as_str()) {
                let text = v
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("<non-text>");
                format!("{role}:{text}")
            } else if ty == "reasoning" {
                let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("?");
                format!("reasoning:{id}")
            } else if ty == "function_call" {
                let cid = v.get("call_id").and_then(|c| c.as_str()).unwrap_or("?");
                format!("function_call:{cid}")
            } else {
                format!("type:{ty}")
            }
        })
        .collect()
}

pub(super) fn assert_prefix_stable(base: &ConversationRequest, extended: &ConversationRequest) {
    let base_input = input_items_json(base);
    let ext_input = input_items_json(extended);
    assert!(
        ext_input.len() >= base_input.len(),
        "extended request has fewer input items ({}) than base ({})",
        ext_input.len(),
        base_input.len(),
    );
    assert_eq!(
        &ext_input[..base_input.len()],
        base_input.as_slice(),
        "serialized input of request N must be a prefix of request N+1.\n\
             Base ({} items): {:?}\nExtended ({} items): {:?}\n\
             First divergence at index {}",
        base_input.len(),
        summarise_input(&base_input),
        ext_input.len(),
        summarise_input(&ext_input),
        base_input
            .iter()
            .zip(ext_input.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(base_input.len()),
    );
}
