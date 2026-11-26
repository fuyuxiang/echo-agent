//! Tests for the Chat Completions conversion.

use super::test_support::*;
use super::*;
use assert_matches::assert_matches;

fn make_test_tool() -> ToolSpec {
    ToolSpec {
        name: "test_tool".to_string(),
        description: Some("A test tool".to_string()),
        parameters: serde_json::json!({}),
    }
}

#[test]
fn test_conversation_item_roundtrip() {
    let system = ConversationItem::system("You are a helpful assistant.");
    let chat_msg = conversation_item_to_chat_message(system.clone());
    let back: ConversationItem = chat_msg.into();
    assert_eq!(back.text_content(), "You are a helpful assistant.");

    let user = ConversationItem::user("Hello!");
    let chat_msg = conversation_item_to_chat_message(user);
    let back: ConversationItem = chat_msg.into();
    assert_eq!(back.text_content(), "Hello!");

    // Assistant message (reasoning is now a sibling, not a field;
    // single-item conversion produces None for reasoning_content. The
    // `conversation_to_chat_messages` helper is what carries reasoning
    // through; tested separately).
    let assistant = ConversationItem::assistant_with_model("Hi there!", "echo-agent-3");
    let chat_msg = conversation_item_to_chat_message(assistant);
    assert_eq!(chat_msg.reasoning_content, None);
    let back: ConversationItem = chat_msg.into();
    assert_eq!(back.text_content(), "Hi there!");

    let tool_result = ConversationItem::tool_result("call_123", "Result data");
    let chat_msg = conversation_item_to_chat_message(tool_result);
    assert_eq!(chat_msg.tool_call_id, Some("call_123".to_string()));
}

#[test]
fn test_conversation_request_to_chat_completion() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("System prompt"),
        ConversationItem::user("User message"),
    ])
    .with_model("echo-agent-3")
    .with_temperature(0.7);

    let chat_req: ChatCompletionRequest = req.into();
    assert_eq!(chat_req.model, Some("echo-agent-3".to_string()));
    assert_eq!(chat_req.temperature, Some(0.7));
    assert_eq!(chat_req.messages.len(), 2);
}

#[test]
fn test_user_with_image() {
    let mut user = ConversationItem::user("Check this image");
    user.add_image("https://example.com/image.png");

    let ConversationItem::User(u) = &user else {
        panic!("Expected User item");
    };
    assert_eq!(u.content.len(), 2);
    assert_matches!(
        &u.content[1],
        ContentPart::Image { url } if url.as_ref() == "https://example.com/image.png"
    );

    // Convert to chat request and verify
    let chat_msg = conversation_item_to_chat_message(user);
    let blocks = chat_msg.content.blocks();
    assert_eq!(blocks.len(), 2);
    assert_matches!(
        &blocks[1],
        ChatContentBlock::ImageUrl { image_url } if image_url.url == "https://example.com/image.png"
    );
}

#[test]
fn test_chat_response_message_to_conversation_item() {
    use crate::types::{ChatResponseMessage, Role, ToolCallFunction, ToolCallResponse};

    // Simple text response
    let response_msg = ChatResponseMessage {
        role: Role::Assistant,
        content: Some("Hello, world!".to_string()),
        reasoning_content: None,
        tool_calls: vec![],
        tool_call_id: None,
        citations: None,
    };

    let item: ConversationItem = response_msg.into();
    assert_eq!(item.text_content(), "Hello, world!");
    assert_eq!(item.role(), Role::Assistant);

    // Response with reasoning
    let response_with_reasoning = ChatResponseMessage {
        role: Role::Assistant,
        content: Some("The answer is 42.".to_string()),
        reasoning_content: Some("Let me think step by step...".to_string()),
        tool_calls: vec![],
        tool_call_id: None,
        citations: None,
    };

    let item: ConversationItem = response_with_reasoning.into();
    let ConversationItem::Assistant(a) = &item else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.content.as_ref(), "The answer is 42.");
    // Reasoning content from a chat-completions ChatResponseMessage is
    // dropped on the single-item `From` path; the streaming consumer
    // produces a sibling `ConversationItem::Reasoning` instead. See
    // the doc comment on `From<ChatResponseMessage>`.

    // Response with tool calls
    let response_with_tools = ChatResponseMessage {
        role: Role::Assistant,
        content: None,
        reasoning_content: None,
        tool_calls: vec![ToolCallResponse {
            id: "call_123".to_string(),
            kind: "function".to_string(),
            function: ToolCallFunction {
                name: "read_file".to_string(),
                arguments: r#"{"path": "/foo.txt"}"#.to_string(),
            },
        }],
        tool_call_id: None,
        citations: None,
    };

    let item: ConversationItem = response_with_tools.into();
    let ConversationItem::Assistant(a) = &item else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.tool_calls.len(), 1);
    assert_eq!(a.tool_calls[0].id.as_ref(), "call_123");
    assert_eq!(a.tool_calls[0].name, "read_file");
}

#[test]
fn test_tool_calls_roundtrip_to_chat_request() {
    let tool_call = ToolCall {
        id: "call_abc123".into(),
        name: "read_file".to_string(),
        arguments: r#"{"path": "/foo.txt", "limit": 100}"#.into(),
    };

    let item = ConversationItem::assistant_tool_calls(vec![tool_call.clone()]);

    let chat_msg = conversation_item_to_chat_message(item.clone());
    assert_eq!(chat_msg.tool_calls.len(), 1);
    assert_eq!(chat_msg.tool_calls[0].id, Some("call_abc123".to_string()));
    assert_eq!(chat_msg.tool_calls[0].function.name, "read_file");
    assert_eq!(
        chat_msg.tool_calls[0].function.arguments,
        r#"{"path": "/foo.txt", "limit": 100}"#
    );

    let back: ConversationItem = chat_msg.into();
    let ConversationItem::Assistant(a) = back else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.tool_calls.len(), 1);
    assert_eq!(a.tool_calls[0].id.as_ref(), "call_abc123");
    assert_eq!(a.tool_calls[0].name, "read_file");
    assert_eq!(
        a.tool_calls[0].arguments.as_ref(),
        r#"{"path": "/foo.txt", "limit": 100}"#
    );
}

#[test]
fn test_multiple_tool_calls_roundtrip() {
    let tool_calls = vec![
        ToolCall {
            id: "call_1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "/a.txt"}"#.into(),
        },
        ToolCall {
            id: "call_2".into(),
            name: "bash".to_string(),
            arguments: r#"{"command": "ls -la"}"#.into(),
        },
        ToolCall {
            id: "call_3".into(),
            name: "grep".to_string(),
            arguments: r#"{"pattern": "TODO", "path": "."}"#.into(),
        },
    ];

    let item = ConversationItem::assistant_tool_calls(tool_calls);

    let chat_msg = conversation_item_to_chat_message(item);
    assert_eq!(chat_msg.tool_calls.len(), 3);
    assert_eq!(chat_msg.tool_calls[0].function.name, "read_file");
    assert_eq!(chat_msg.tool_calls[1].function.name, "bash");
    assert_eq!(chat_msg.tool_calls[2].function.name, "grep");

    // Back to ConversationItem
    let back: ConversationItem = chat_msg.into();
    let ConversationItem::Assistant(a) = back else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.tool_calls.len(), 3);
    assert_eq!(a.tool_calls[0].name, "read_file");
    assert_eq!(a.tool_calls[1].name, "bash");
    assert_eq!(a.tool_calls[2].name, "grep");
}

#[test]
fn test_assistant_with_content_and_tool_calls() {
    // Assistant can have both text content and tool calls
    let assistant = AssistantItem {
        content: "Let me help you with that.".into(),
        tool_calls: vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "/test.txt"}"#.into(),
        }],
        model_id: Some("echo-agent-3".to_string()),
        model_fingerprint: None,
        reasoning_effort: None,
    };

    let item = ConversationItem::Assistant(assistant.clone());
    let chat_msg = conversation_item_to_chat_message(item);

    assert_eq!(chat_msg.text_content(), "Let me help you with that.");
    assert_eq!(chat_msg.tool_calls.len(), 1);
    assert_eq!(chat_msg.model_id, Some("echo-agent-3".to_string()));
}

#[test]
fn test_conversation_request_with_tools_to_chat_completion() {
    let tools = vec![
        ToolSpec {
            name: "read_file".to_string(),
            description: Some("Read a file from disk".to_string()),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "bash".to_string(),
            description: Some("Run a bash command".to_string()),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        },
    ];

    let req =
        ConversationRequest::from_items(vec![ConversationItem::user("Help me")]).with_tools(tools);

    let chat_req: ChatCompletionRequest = req.into();
    assert!(chat_req.tools.is_some());
    let tools = chat_req.tools.unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].function.name, "read_file");
    assert_eq!(tools[1].function.name, "bash");
}

#[test]
fn tool_choice_presets_map_to_wire_strings() {
    for (choice, wire) in [
        (ConversationToolChoice::Auto, "auto"),
        (ConversationToolChoice::None, "none"),
        (ConversationToolChoice::Required, "required"),
    ] {
        let req = ConversationRequest::from_items(vec![ConversationItem::user("test")])
            .with_tools(vec![make_test_tool()])
            .with_tool_choice(choice);

        let chat_req: ChatCompletionRequest = req.into();
        let Some(ToolChoice::Preset(preset)) = chat_req.tool_choice else {
            panic!("expected a preset tool choice for {wire}");
        };
        assert_eq!(preset, wire);
    }
}

#[test]
fn test_tool_choice_function_to_chat_completion() {
    let req = ConversationRequest::from_items(vec![ConversationItem::user("test")])
        .with_tools(vec![make_test_tool()])
        .with_tool_choice(ConversationToolChoice::Function("read_file".to_string()));

    let chat_req: ChatCompletionRequest = req.into();
    let ToolChoice::Function { function, .. } = chat_req.tool_choice.unwrap() else {
        panic!("Expected Function tool choice");
    };
    assert_eq!(function.name, "read_file");
}

#[test]
fn test_tool_choice_dropped_when_no_tools_chat_completions() {
    // Chat Completions API rejects tool_choice without tools
    let req = ConversationRequest::from_items(vec![ConversationItem::user("test")])
        .with_tool_choice(ConversationToolChoice::Auto);
    let chat_req: ChatCompletionRequest = req.into();
    assert!(chat_req.tool_choice.is_none());
    assert!(chat_req.tools.is_none());
}

#[test]
fn test_user_with_multiple_images() {
    let parts = vec![
        ContentPart::Text {
            text: "Compare these images:".into(),
        },
        ContentPart::Image {
            url: "https://example.com/img1.png".into(),
        },
        ContentPart::Image {
            url: "https://example.com/img2.png".into(),
        },
        ContentPart::Image {
            url: "data:image/png;base64,iVBORw0KGgo=".into(),
        },
    ];

    let user = ConversationItem::user_with_parts(parts);

    let chat_msg = conversation_item_to_chat_message(user);
    let blocks = chat_msg.content.blocks();
    assert_eq!(blocks.len(), 4);
    assert_matches!(&blocks[0], ChatContentBlock::Text { text } if text == "Compare these images:");
    assert_matches!(&blocks[1], ChatContentBlock::ImageUrl { .. });
    assert_matches!(&blocks[2], ChatContentBlock::ImageUrl { .. });
    assert_matches!(&blocks[3], ChatContentBlock::ImageUrl { .. });
}

#[test]
fn test_malformed_tool_arguments_sanitized_to_empty_object_in_chat_request() {
    // Exactly the broken string from the real incident:
    // missing `"` before `new_string` → JSON parse fails at char 80.
    let bad_args = r#"{"file_path": "/testbed/cxx_polynomial/include/emsr/remez.h", "old_string": "", new_string": "x"}"#;
    assert!(
        serde_json::from_str::<serde_json::Value>(bad_args).is_err(),
        "pre-condition: bad_args must be invalid JSON"
    );

    let tool_call = ToolCall {
        id: "functions.search_replace:10".into(),
        name: "search_replace".to_string(),
        arguments: bad_args.into(),
    };

    let item = ConversationItem::assistant_tool_calls(vec![tool_call]);
    let chat_msg = conversation_item_to_chat_message(item);

    // Arguments must be replaced with valid JSON.
    let sanitized = &chat_msg.tool_calls[0].function.arguments;
    assert_eq!(
        sanitized, "{}",
        "malformed arguments must be replaced with {{}}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(sanitized).is_ok(),
        "sanitized arguments must be valid JSON"
    );
}

#[test]
fn test_valid_tool_arguments_pass_through_unchanged_in_chat_request() {
    let valid_args = r#"{"file_path": "/foo.rs", "old_string": "a", "new_string": "b"}"#;
    let tool_call = ToolCall {
        id: "call_1".into(),
        name: "search_replace".to_string(),
        arguments: valid_args.into(),
    };

    let item = ConversationItem::assistant_tool_calls(vec![tool_call]);
    let chat_msg = conversation_item_to_chat_message(item);
    assert_eq!(
        chat_msg.tool_calls[0].function.arguments, valid_args,
        "valid arguments must not be modified"
    );
}

#[test]
fn test_chat_completion_request_carries_reasoning_effort_top_level() {
    for (variant, expected) in [
        (crate::ReasoningEffort::None, "none"),
        (crate::ReasoningEffort::Minimal, "minimal"),
        (crate::ReasoningEffort::Low, "low"),
        (crate::ReasoningEffort::Medium, "medium"),
        (crate::ReasoningEffort::High, "high"),
        (crate::ReasoningEffort::Xhigh, "xhigh"),
        (crate::ReasoningEffort::Max, "max"),
    ] {
        let req =
            ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_model("test");
        let req = ConversationRequest {
            reasoning_effort: Some(variant),
            ..req
        };
        let chat: ChatCompletionRequest = req.into();
        let json = serde_json::to_value(&chat).unwrap();
        assert_eq!(
            json.pointer("/reasoning_effort").and_then(|v| v.as_str()),
            Some(expected),
            "{variant:?} should serialize as top-level reasoning_effort={expected:?}; got: {json:#}",
        );
    }
}

#[test]
fn test_chat_completion_request_omits_reasoning_effort_when_unset() {
    let req =
        ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_model("test");
    let chat: ChatCompletionRequest = req.into();
    let json = serde_json::to_value(&chat).unwrap();
    assert!(
        json.get("reasoning_effort").is_none(),
        "reasoning_effort must be absent when unset; got: {json:#}",
    );
}

#[test]
fn test_btw_cross_api_chat_completions_no_regressions() {
    let items = btw_prepare_items(btw_mid_turn_conversation());
    let req = ConversationRequest::from_items(items);
    let chat: ChatCompletionRequest = req.into();
    let json = serde_json::to_value(&chat).unwrap();

    let messages = json.get("messages").unwrap().as_array().unwrap();

    // Last assistant must not have orphaned tool_calls.
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .expect("should have an assistant message");
    let has_tool_calls = last_assistant
        .get("tool_calls")
        .and_then(|tc| tc.as_array())
        .is_some_and(|a| !a.is_empty());
    // If the last assistant has tool_calls, there must be a tool message after it.
    if has_tool_calls {
        let last_asst_idx = messages
            .iter()
            .rposition(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
            .unwrap();
        let has_following_tool = messages[last_asst_idx + 1..]
            .iter()
            .any(|m| m.get("role").and_then(|r| r.as_str()) == Some("tool"));
        assert!(
            has_following_tool,
            "last assistant with tool_calls must have a following tool message"
        );
    }

    // Temperature must be absent.
    assert!(
        json.get("temperature").is_none()
            || json.pointer("/temperature").is_some_and(|v| v.is_null()),
        "temperature must be absent; got: {json:#}",
    );

    // The completed tool pair (call_1) must survive.
    let has_call_1 = messages.iter().any(|m| {
        m.get("tool_calls")
            .and_then(|tc| tc.as_array())
            .is_some_and(|calls| {
                calls
                    .iter()
                    .any(|c| c.get("id").and_then(|id| id.as_str()) == Some("call_1"))
            })
    });
    assert!(has_call_1, "completed tool_call call_1 must survive");

    let has_tool_result_1 = messages.iter().any(|m| {
        m.get("role").and_then(|r| r.as_str()) == Some("tool")
            && m.get("tool_call_id").and_then(|id| id.as_str()) == Some("call_1")
    });
    assert!(
        has_tool_result_1,
        "completed tool result for call_1 must survive"
    );
}

#[test]
fn test_sanitize_non_ascii_args_preview_does_not_panic() {
    // Build a string where the 200-byte boundary lands inside a CJK char.
    // Each '文' is 3 bytes → 67 × 3 = 201 bytes; byte 200 is inside the 67th char.
    let filler = "文".repeat(70); // > 200 bytes
    let bad_args = format!("{{\"old_string\": \"{filler}\"}}");
    // The outer JSON is valid but contains non-ASCII; force the warning path
    // by making the JSON invalid.
    let malformed = format!("{{\"old_string\": \"{filler}\" missing_key}}");

    let tool_call = ToolCall {
        id: "call_1".into(),
        name: "search_replace".to_string(),
        arguments: malformed.clone().into(),
    };
    // Must not panic.
    let item = ConversationItem::assistant_tool_calls(vec![tool_call]);
    let chat_msg = conversation_item_to_chat_message(item);
    assert_eq!(
        chat_msg.tool_calls[0].function.arguments, "{}",
        "malformed non-ASCII arguments must be sanitized to {{}}"
    );
    // Also confirm valid non-ASCII passes through unchanged.
    let tool_call_valid = ToolCall {
        id: "call_2".into(),
        name: "search_replace".to_string(),
        arguments: bad_args.clone().into(),
    };
    let item_valid = ConversationItem::assistant_tool_calls(vec![tool_call_valid]);
    let chat_msg_valid = conversation_item_to_chat_message(item_valid);
    assert_eq!(
        chat_msg_valid.tool_calls[0].function.arguments, bad_args,
        "valid non-ASCII arguments must pass through unchanged"
    );
}

#[test]
fn test_tool_result_with_images_to_chat_completions() {
    let item = ConversationItem::tool_result_with_images(
        "call_1",
        "Read image file: photo.png",
        vec![ContentPart::Image {
            url: "data:image/png;base64,iVBOR".into(),
        }],
    );

    let msg = conversation_item_to_chat_message(item);
    assert_eq!(msg.role, Role::Tool);
    assert_eq!(msg.tool_call_id, Some("call_1".to_string()));

    // Should be Blocks, not Text
    let MessageContent::Blocks(blocks) = &msg.content else {
        panic!(
            "Expected Blocks content for image tool result, got {:?}",
            msg.content
        );
    };
    assert_eq!(blocks.len(), 2);
    assert!(
        matches!(&blocks[0], ChatContentBlock::Text { text } if text == "Read image file: photo.png")
    );
    assert!(
        matches!(&blocks[1], ChatContentBlock::ImageUrl { image_url } if image_url.url == "data:image/png;base64,iVBOR")
    );
}

#[test]
fn conversation_to_chat_messages_drops_reasoning_when_user_intervenes() {
    // Reasoning only folds onto the *immediately* following assistant. A
    // non-assistant item in between (here a User) clears pending reasoning,
    // matching the "reasoning lived on the immediately-following assistant
    // turn only" semantic. This is the non-trailing sibling of
    // `conversation_to_chat_messages_drops_trailing_reasoning`.
    let items = vec![
        reasoning_sibling("r1", "stale thinking", None),
        ConversationItem::user("actually, new question"),
        ConversationItem::assistant("answer"),
    ];

    let msgs = conversation_to_chat_messages(items);

    assert_eq!(
        msgs.len(),
        2,
        "user + assistant; orphaned reasoning dropped"
    );
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(msgs[1].role, Role::Assistant);
    assert_eq!(msgs[1].text_content(), "answer");
    assert_eq!(
        msgs[1].reasoning_content.as_deref(),
        None,
        "reasoning separated from the assistant by a user message is dropped"
    );
}

#[test]
fn upgrade_then_fold_through_conversation_to_chat_messages() {
    // End-to-end: lift legacy `reasoning` to a sibling, then run the
    // chat-completions wire path. Reasoning must land on the next
    // assistant's `reasoning_content`. This mirrors what the real
    // load-then-replay flow does for a legacy session.
    let raw = serde_json::json!({
        "type": "assistant",
        "content": "the answer",
        "reasoning": {"text": "step-by-step", "id": "rs_x"}
    });
    let mut seen = std::collections::HashSet::new();
    let mut siblings = upgrade_legacy_reasoning(&raw, &mut seen);
    // Append the assistant (post-strip) by re-deserializing the same
    // raw value as the new AssistantItem (which silently ignores
    // `reasoning`).
    let assistant: ConversationItem = serde_json::from_value(raw).unwrap();
    siblings.push(assistant);

    let msgs = conversation_to_chat_messages(siblings);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, Role::Assistant);
    assert_eq!(
        msgs[0].reasoning_content.as_deref(),
        Some("step-by-step"),
        "reconstructed sibling folded onto assistant.reasoning_content"
    );
}
