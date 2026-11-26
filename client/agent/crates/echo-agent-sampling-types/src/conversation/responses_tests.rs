//! Tests for the Responses API conversion.

use super::test_support::*;
use super::*;
use crate::tool_overrides::*;
use assert_matches::assert_matches;

#[test]
fn test_conversation_request_to_responses_api() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("System prompt"),
        ConversationItem::user("User message"),
    ])
    .with_model("echo-agent-3")
    .with_temperature(0.7);

    let responses_req: rs::CreateResponse = (&req).into();
    assert_eq!(responses_req.model, Some("echo-agent-3".to_string()));
    assert_eq!(responses_req.temperature, Some(0.7));

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    assert_eq!(items.len(), 2);
}

#[test]
fn function_tool_colliding_with_hosted_web_search_is_dropped() {
    let mut req =
        ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_tools(vec![
            ToolSpec {
                name: "web_search".to_string(),
                description: Some("local web search".to_string()),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolSpec {
                name: "read_file".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object"}),
            },
        ]);
    req.hosted_tools = vec![HostedTool::WebSearch { options: None }];

    let responses_req: rs::CreateResponse = (&req).into();
    let tools = responses_req.tools.expect("tools should be set");

    // web_search rides the raw-JSON `extra_tool_entries` channel (so it can carry
    // `excluded_domains`, which async_openai's typed filter omits), so it never
    // appears as a native `rs::Tool::WebSearch`.
    let web_search_count = tools
        .iter()
        .filter(|t| matches!(t, rs::Tool::WebSearch(_)))
        .count();
    assert_eq!(
        web_search_count, 0,
        "web_search is not a native tool: {tools:?}"
    );
    let function_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| match t {
            rs::Tool::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        function_names,
        vec!["read_file"],
        "colliding function tool must be dropped"
    );

    // The hosted web_search is emitted as a raw entry instead.
    let entries = extra_tool_entries(&req.hosted_tools);
    assert_eq!(entries, vec![serde_json::json!({"type": "web_search"})]);
}

#[test]
fn function_tool_colliding_with_hosted_x_search_is_dropped() {
    let mut req =
        ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_tools(vec![
            ToolSpec {
                name: "x_search".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object"}),
            },
        ]);
    req.hosted_tools = vec![HostedTool::XSearch { options: None }];

    let responses_req: rs::CreateResponse = (&req).into();
    let tools = responses_req.tools.unwrap_or_default();
    assert!(tools.is_empty(), "expected no tools, got: {tools:?}");
    let entries = extra_tool_entries(&req.hosted_tools);
    assert_eq!(entries, vec![serde_json::json!({"type": "x_search"})]);
}

/// The hosted `web_search` domain policy only reaches the API through this raw
/// entry (async_openai's typed filters model no blocklist), so both filters must
/// survive the `HostedTool` → `extra_tool_entries` hop, and an empty/absent
/// policy must stay byte-identical to the bare tool.
#[test]
fn web_search_domain_filters_reach_the_tool_entry() {
    let hosted = |options: Option<WebSearchOptions>| {
        extra_tool_entries(&[HostedTool::WebSearch { options }])
    };
    assert_eq!(
        hosted(Some(WebSearchOptions {
            allowed_domains: Some(vec!["docs.echo-agent.invalid".into(), "arxiv.org".into()]),
            excluded_domains: None,
        })),
        vec![serde_json::json!({
            "type": "web_search",
            "filters": { "allowed_domains": ["docs.echo-agent.invalid", "arxiv.org"] },
        })]
    );
    assert_eq!(
        hosted(Some(WebSearchOptions {
            allowed_domains: None,
            excluded_domains: Some(vec!["reddit.com".into()]),
        })),
        vec![serde_json::json!({
            "type": "web_search",
            "filters": { "excluded_domains": ["reddit.com"] },
        })]
    );

    // No policy (absent, default, or empty lists) emits the bare tool.
    let bare = vec![serde_json::json!({ "type": "web_search" })];
    assert_eq!(hosted(None), bare);
    assert_eq!(hosted(Some(WebSearchOptions::default())), bare);
    assert_eq!(
        hosted(Some(WebSearchOptions {
            allowed_domains: Some(vec![]),
            excluded_domains: Some(vec![]),
        })),
        bare
    );
}

#[test]
fn x_search_serializes_to_the_tool_entry() {
    // A full bound reaches the flat snake_case entry; an empty or `None` bound emits the bare entry.
    let dated = extra_tool_entries(&[HostedTool::XSearch {
        options: Some(XSearchOptions {
            date_bound: Some(
                SearchDateBound::new(Some("2024-01-01".into()), Some("2024-03-15".into())).unwrap(),
            ),
        }),
    }]);
    assert_eq!(
        dated,
        vec![serde_json::json!({
            "type": "x_search",
            "from_date": "2024-01-01",
            "to_date": "2024-03-15",
        })]
    );
    let bare = vec![serde_json::json!({"type": "x_search"})];
    assert_eq!(
        extra_tool_entries(&[HostedTool::XSearch {
            options: Some(XSearchOptions {
                date_bound: Some(SearchDateBound::new(None, None).unwrap()),
            }),
        }]),
        bare
    );
    assert_eq!(
        extra_tool_entries(&[HostedTool::XSearch { options: None }]),
        bare
    );
}

#[test]
fn function_web_search_kept_when_no_hosted_tools() {
    let req = ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_tools(vec![
        ToolSpec {
            name: "web_search".to_string(),
            description: None,
            parameters: serde_json::json!({"type": "object"}),
        },
    ]);

    let responses_req: rs::CreateResponse = (&req).into();
    let tools = responses_req.tools.expect("tools should be set");
    let function_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| match t {
            rs::Tool::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(function_names, vec!["web_search"]);
}

#[test]
fn test_responses_api_response_to_conversation_item() {
    use crate::rs;

    // Create a Response with text output
    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_123".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "echo-agent-3".to_string(),
        object: "response".to_string(),
        output: vec![rs::OutputItem::Message(rs::OutputMessage {
            content: vec![rs::OutputMessageContent::OutputText(
                rs::OutputTextContent {
                    text: "Hello from Responses API!".to_string(),
                    annotations: vec![],
                    logprobs: None,
                },
            )],
            id: "msg_123".to_string(),
            role: rs::AssistantRole::Assistant,
            status: rs::OutputStatus::Completed,
        })],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    let items = response_to_conversation_items(response);
    let item = items
        .into_iter()
        .next_back()
        .expect("response produces at least a trailing Assistant");
    assert_eq!(item.text_content(), "Hello from Responses API!");
    let ConversationItem::Assistant(a) = &item else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.model_id, Some("echo-agent-3".to_string()));
    assert_eq!(
        a.reasoning_effort, None,
        "no reasoning config on the response => no effort recorded"
    );

    // Response with function call
    let response_with_fc = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_456".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "echo-agent-3".to_string(),
        object: "response".to_string(),
        output: vec![rs::OutputItem::FunctionCall(rs::FunctionToolCall {
            arguments: r#"{"path": "/bar.txt"}"#.to_string(),
            call_id: "call_789".to_string(),
            name: "read_file".to_string(),
            id: None,
            status: None,
        })],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    let items = response_to_conversation_items(response_with_fc);
    let item = items
        .into_iter()
        .next_back()
        .expect("response produces at least a trailing Assistant");
    let ConversationItem::Assistant(a) = &item else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.tool_calls.len(), 1);
    assert_eq!(a.tool_calls[0].id.as_ref(), "call_789");
    assert_eq!(a.tool_calls[0].name, "read_file");
}

#[test]
fn test_response_reasoning_effort_stamped_on_assistant() {
    use crate::rs;

    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_eff".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "echo-agent-3".to_string(),
        object: "response".to_string(),
        output: vec![],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: Some(rs::Reasoning {
            effort: Some(rs::ReasoningEffort::Xhigh),
            summary: None,
        }),
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    let items = response_to_conversation_items(response);
    let ConversationItem::Assistant(a) = items.last().expect("trailing Assistant") else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.reasoning_effort, Some(crate::ReasoningEffort::Xhigh));

    // Round-trips through the persisted representation.
    let json = serde_json::to_string(&items.last().unwrap()).unwrap();
    assert!(json.contains(r#""reasoning_effort":"xhigh""#), "{json}");
    let back: ConversationItem = serde_json::from_str(&json).unwrap();
    let ConversationItem::Assistant(b) = back else {
        panic!("Expected Assistant item");
    };
    assert_eq!(b.reasoning_effort, Some(crate::ReasoningEffort::Xhigh));
}

#[test]
fn test_tool_calls_to_responses_api() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("System"),
        ConversationItem::user("User"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "bash".to_string(),
            arguments: r#"{"command": "ls"}"#.into(),
        }]),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    // Should have: system message, user message, (possibly assistant text), function_call
    // Find the FunctionCall item
    let fc_items: Vec<_> = items
        .iter()
        .filter(|item| matches!(item, rs::InputItem::Item(rs::Item::FunctionCall(_))))
        .collect();

    assert_eq!(fc_items.len(), 1, "Expected exactly one FunctionCall item");

    let rs::InputItem::Item(rs::Item::FunctionCall(fc)) = fc_items[0] else {
        panic!("Expected FunctionCall item");
    };
    assert_eq!(fc.call_id, "call_1");
    assert_eq!(fc.name, "bash");
    assert_eq!(fc.arguments, r#"{"command": "ls"}"#);
}

#[test]
fn test_tool_result_to_responses_api() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("System"),
        ConversationItem::user("User"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "bash".to_string(),
            arguments: r#"{"command": "ls"}"#.into(),
        }]),
        ConversationItem::tool_result("call_1", "file1.txt\nfile2.txt\nfile3.txt"),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    // Find the FunctionCallOutput item
    let fco_items: Vec<_> = items
        .iter()
        .filter(|item| matches!(item, rs::InputItem::Item(rs::Item::FunctionCallOutput(_))))
        .collect();

    assert_eq!(
        fco_items.len(),
        1,
        "Expected exactly one FunctionCallOutput item"
    );

    let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = fco_items[0] else {
        panic!("Expected FunctionCallOutput item");
    };
    assert_eq!(fco.call_id, "call_1");
    let rs::FunctionCallOutput::Text(text) = &fco.output else {
        panic!("Expected Text output");
    };
    assert_eq!(text, "file1.txt\nfile2.txt\nfile3.txt");
}

#[test]
fn test_multiple_tool_results_to_responses_api() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Run these commands"),
        ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "call_1".into(),
                name: "bash".to_string(),
                arguments: r#"{"command": "ls"}"#.into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "bash".to_string(),
                arguments: r#"{"command": "pwd"}"#.into(),
            },
        ]),
        ConversationItem::tool_result("call_1", "output1"),
        ConversationItem::tool_result("call_2", "output2"),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let fco_items: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = item {
                Some(fco)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(fco_items.len(), 2);
    assert_eq!(fco_items[0].call_id, "call_1");
    assert_eq!(fco_items[1].call_id, "call_2");
}

#[test]
fn test_responses_api_with_reasoning() {
    // Test conversion from Responses API response with reasoning
    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_123".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "echo-agent-3".to_string(),
        object: "response".to_string(),
        output: vec![
            rs::OutputItem::Reasoning(rs::ReasoningItem {
                id: "reasoning_1".to_string(),
                summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text: "I need to analyze this carefully.".to_string(),
                })],
                content: None,
                encrypted_content: None,
                status: None,
            }),
            rs::OutputItem::Message(rs::OutputMessage {
                content: vec![rs::OutputMessageContent::OutputText(
                    rs::OutputTextContent {
                        text: "Here is my answer.".to_string(),
                        annotations: vec![],
                        logprobs: None,
                    },
                )],
                id: "msg_123".to_string(),
                role: rs::AssistantRole::Assistant,
                status: rs::OutputStatus::Completed,
            }),
        ],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    // The full flat-list shape (Reasoning siblings preserved) is
    // exercised in the `test_response_to_conversation_items_preserves_*`
    // tests below; here we just assert the trailing Assistant content.
    let items = response_to_conversation_items(response);
    let item = items
        .into_iter()
        .next_back()
        .expect("response produces at least a trailing Assistant");
    let ConversationItem::Assistant(a) = &item else {
        panic!("Expected Assistant item");
    };
    assert_eq!(a.content.as_ref(), "Here is my answer.");
}

#[test]
fn test_responses_api_with_encrypted_reasoning() {
    // Test that encrypted reasoning content is preserved from Responses API
    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_456".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "echo-agent-3".to_string(),
        object: "response".to_string(),
        output: vec![
            rs::OutputItem::Reasoning(rs::ReasoningItem {
                id: "reasoning_enc".to_string(),
                summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text: "Visible thinking summary".to_string(),
                })],
                content: None,
                encrypted_content: Some("enc_base64_encrypted_reasoning_data_here".to_string()),
                status: Some(rs::OutputStatus::Completed),
            }),
            rs::OutputItem::Message(rs::OutputMessage {
                content: vec![rs::OutputMessageContent::OutputText(
                    rs::OutputTextContent {
                        text: "My response based on reasoning.".to_string(),
                        annotations: vec![],
                        logprobs: None,
                    },
                )],
                id: "msg_456".to_string(),
                role: rs::AssistantRole::Assistant,
                status: rs::OutputStatus::Completed,
            }),
        ],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    // Exercise the flat-list path: reasoning now lives as a sibling.
    let items = response_to_conversation_items(response);
    let assistant_idx = items
        .iter()
        .position(|i| matches!(i, ConversationItem::Assistant(_)))
        .expect("assistant present");
    let ConversationItem::Assistant(a) = &items[assistant_idx] else {
        unreachable!()
    };
    assert_eq!(a.content.as_ref(), "My response based on reasoning.");

    let reasoning_sibling = items
        .iter()
        .find_map(|i| match i {
            ConversationItem::Reasoning(r) => Some(r),
            _ => None,
        })
        .expect("reasoning sibling present");
    // Both text summary and encrypted content should be preserved
    assert_eq!(
        reasoning_sibling.summary.first().map(|sp| match sp {
            rs::SummaryPart::SummaryText(t) => t.text.as_str(),
        }),
        Some("Visible thinking summary")
    );
    assert_eq!(
        reasoning_sibling.encrypted_content.as_deref(),
        Some("enc_base64_encrypted_reasoning_data_here")
    );
}

#[test]
fn test_responses_api_with_only_encrypted_reasoning() {
    // Test case where there's only encrypted content, no visible summary
    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 1234567890,
        completed_at: None,
        error: None,
        id: "resp_789".to_string(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: None,
        model: "echo-agent-3".to_string(),
        object: "response".to_string(),
        output: vec![
            rs::OutputItem::Reasoning(rs::ReasoningItem {
                id: "reasoning_only_enc".to_string(),
                summary: vec![], // Empty summary
                content: None,
                encrypted_content: Some("enc_only_encrypted_no_visible_summary".to_string()),
                status: Some(rs::OutputStatus::Completed),
            }),
            rs::OutputItem::Message(rs::OutputMessage {
                content: vec![rs::OutputMessageContent::OutputText(
                    rs::OutputTextContent {
                        text: "Response.".to_string(),
                        annotations: vec![],
                        logprobs: None,
                    },
                )],
                id: "msg_789".to_string(),
                role: rs::AssistantRole::Assistant,
                status: rs::OutputStatus::Completed,
            }),
        ],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    // Flat-list path: reasoning sibling carries the encrypted blob,
    // empty summary maps to an empty `Vec<SummaryPart>`.
    let items = response_to_conversation_items(response);
    let reasoning_sibling = items
        .iter()
        .find_map(|i| match i {
            ConversationItem::Reasoning(r) => Some(r),
            _ => None,
        })
        .expect("reasoning sibling present");
    assert!(reasoning_sibling.summary.is_empty());
    assert_eq!(
        reasoning_sibling.encrypted_content.as_deref(),
        Some("enc_only_encrypted_no_visible_summary")
    );
}

#[test]
fn test_conversation_item_with_sibling_reasoning_serialization() {
    // Reasoning is now a sibling variant — round-trip both items
    // through serde and confirm they survive.
    let reasoning_item = ConversationItem::Reasoning(rs::ReasoningItem {
        id: "reasoning_1".to_string(),
        summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
            text: "Computing the answer...".to_string(),
        })],
        content: None,
        encrypted_content: Some("enc_ultimate_answer_computation".to_string()),
        status: None,
    });
    let assistant_item = ConversationItem::Assistant(AssistantItem {
        content: "The answer is 42.".into(),
        tool_calls: vec![],
        model_id: Some("echo-agent-3".to_string()),
        model_fingerprint: None,
        reasoning_effort: None,
    });

    for item in [reasoning_item, assistant_item] {
        let json = serde_json::to_string(&item).expect("Should serialize");
        let back: ConversationItem = serde_json::from_str(&json).expect("Should deserialize");
        assert_eq!(std::mem::discriminant(&item), std::mem::discriminant(&back));
    }
}

#[test]
fn test_encrypted_reasoning_included_in_responses_api_request() {
    // Test that when building a Responses API request, encrypted reasoning is included
    // This is crucial for context continuity across turns
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("You are helpful"),
        ConversationItem::user("What is 2+2?"),
        // Previous reasoning + assistant: reasoning is now a sibling.
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "r1".to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: "Let me calculate 2+2...".to_string(),
            })],
            content: None,
            encrypted_content: Some("enc_secret_reasoning_chain".to_string()),
            status: None,
        }),
        ConversationItem::Assistant(AssistantItem {
            content: "The answer is 4.".into(),
            tool_calls: vec![],
            model_id: Some("echo-agent-3".to_string()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        // New user message
        ConversationItem::user("Now what is 3+3?"),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    // Find the reasoning item in the input
    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let reasoning_items: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::Reasoning(r)) = item {
                Some(r)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        reasoning_items.len(),
        1,
        "Should have exactly one reasoning item"
    );

    let reasoning = reasoning_items[0];
    // Verify encrypted content is included
    assert_eq!(
        reasoning.encrypted_content,
        Some("enc_secret_reasoning_chain".to_string())
    );

    // Verify summary text is included
    assert_eq!(reasoning.summary.len(), 1);
    let rs::SummaryPart::SummaryText(summary) = &reasoning.summary[0];
    assert_eq!(summary.text, "Let me calculate 2+2...");
}

#[test]
fn test_only_encrypted_reasoning_included_in_request() {
    // Test that when there's only encrypted content (no visible summary),
    // it's still included in the request
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Hello"),
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: String::new(),
            summary: vec![],
            content: None,
            encrypted_content: Some("enc_hidden_thoughts".to_string()),
            status: None,
        }),
        ConversationItem::Assistant(AssistantItem {
            content: "Hi!".into(),
            tool_calls: vec![],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let reasoning_items: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::Reasoning(r)) = item {
                Some(r)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(reasoning_items.len(), 1);
    let reasoning = reasoning_items[0];

    // Encrypted content should be present
    assert_eq!(
        reasoning.encrypted_content,
        Some("enc_hidden_thoughts".to_string())
    );

    // Summary should be empty
    assert!(reasoning.summary.is_empty());
}

#[test]
fn test_no_reasoning_item_when_no_reasoning() {
    // Test that when there's no reasoning, no reasoning item is added
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Hello"),
        ConversationItem::assistant("Hi!"), // No reasoning
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let reasoning_items: Vec<_> = items
        .iter()
        .filter(|item| matches!(item, rs::InputItem::Item(rs::Item::Reasoning(_))))
        .collect();

    assert!(reasoning_items.is_empty(), "Should have no reasoning items");
}

#[test]
fn test_conversation_request_with_tools_to_responses_api() {
    let tools = vec![ToolSpec {
        name: "search".to_string(),
        description: Some("Search the codebase".to_string()),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            }
        }),
    }];

    let req = ConversationRequest::from_items(vec![ConversationItem::user("Find TODO comments")])
        .with_tools(tools);

    let responses_req: rs::CreateResponse = (&req).into();
    assert!(responses_req.tools.is_some());
    let tools = responses_req.tools.unwrap();
    assert_eq!(tools.len(), 1);

    let rs::Tool::Function(ft) = &tools[0] else {
        panic!("Expected Function tool");
    };
    assert_eq!(ft.name, "search");
    assert_eq!(ft.description, Some("Search the codebase".to_string()));
}

#[test]
fn test_tool_choice_to_responses_api() {
    // Test Auto
    let req = ConversationRequest::from_items(vec![ConversationItem::user("test")])
        .with_tool_choice(ConversationToolChoice::Auto);
    let responses_req: rs::CreateResponse = (&req).into();
    assert_matches!(
        responses_req.tool_choice,
        Some(rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::Auto))
    );

    // Test Required
    let req = ConversationRequest::from_items(vec![ConversationItem::user("test")])
        .with_tool_choice(ConversationToolChoice::Required);
    let responses_req: rs::CreateResponse = (&req).into();
    assert_matches!(
        responses_req.tool_choice,
        Some(rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::Required))
    );

    // Test Function
    let req = ConversationRequest::from_items(vec![ConversationItem::user("test")])
        .with_tool_choice(ConversationToolChoice::Function("bash".to_string()));
    let responses_req: rs::CreateResponse = (&req).into();
    let Some(rs::ToolChoiceParam::Function(fc)) = responses_req.tool_choice else {
        panic!("Expected Function tool choice");
    };
    assert_eq!(fc.name, "bash");
}

#[test]
fn test_malformed_tool_arguments_sanitized_in_responses_api() {
    let bad_args = r#"{"file_path": "/testbed/cxx_polynomial/include/emsr/remez.h", "old_string": "", new_string": "x"}"#;

    let tool_call = ToolCall {
        id: "call_bad".into(),
        name: "search_replace".to_string(),
        arguments: bad_args.into(),
    };

    let item = ConversationItem::assistant_tool_calls(vec![tool_call]);
    let req = ConversationRequest {
        items: vec![item],
        ..Default::default()
    };

    let rs_req: crate::rs::CreateResponse = (&req).into();

    // The FunctionCall input item must carry sanitized arguments.
    let crate::rs::InputParam::Items(items) = rs_req.input else {
        panic!("Expected InputParam::Items");
    };
    let fc_args = items.iter().find_map(|inp| {
        if let crate::rs::InputItem::Item(crate::rs::Item::FunctionCall(fc)) = inp {
            Some(fc.arguments.clone())
        } else {
            None
        }
    });

    let fc_args = fc_args.expect("should find a FunctionCall input item");
    assert_eq!(
        fc_args, "{}",
        "malformed arguments must be replaced with {{}} in Responses API path"
    );
}

#[test]
fn test_responses_request_carries_reasoning_effort_nested() {
    for (variant, expected) in [
        (crate::ReasoningEffort::None, "none"),
        (crate::ReasoningEffort::Minimal, "minimal"),
        (crate::ReasoningEffort::Low, "low"),
        (crate::ReasoningEffort::Medium, "medium"),
        (crate::ReasoningEffort::High, "high"),
        (crate::ReasoningEffort::Xhigh, "xhigh"),
        (crate::ReasoningEffort::Max, "max"),
    ] {
        let req = ConversationRequest {
            reasoning_effort: Some(variant),
            ..ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_model("test")
        };
        let resp: crate::rs::CreateResponse = (&req).into();
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(
            json.pointer("/reasoning/effort").and_then(|v| v.as_str()),
            Some(expected),
            "{variant:?} should serialize as reasoning.effort={expected:?}; got: {json:#}",
        );
    }
}

#[test]
fn test_responses_request_omits_effort_when_unset() {
    let req =
        ConversationRequest::from_items(vec![ConversationItem::user("hi")]).with_model("test");
    let resp: crate::rs::CreateResponse = (&req).into();
    let json = serde_json::to_value(&resp).unwrap();
    assert!(
        json.pointer("/reasoning/effort").is_none(),
        "reasoning.effort must be absent when unset; got: {json:#}",
    );
}

#[test]
fn test_btw_cross_api_responses_no_regressions() {
    let items = btw_prepare_items(btw_mid_turn_conversation());
    let req = ConversationRequest::from_items(items);
    let resp: rs::CreateResponse = (&req).into();
    let json = serde_json::to_value(&resp).unwrap();

    let rs::InputParam::Items(input_items) = &resp.input else {
        panic!("Expected InputParam::Items");
    };

    // Count FunctionCall and FunctionCallOutput items.
    let function_calls: Vec<_> = input_items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCall(fc)) = item {
                Some(fc.call_id.clone())
            } else {
                None
            }
        })
        .collect();
    let function_outputs: Vec<_> = input_items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = item {
                Some(fco.call_id.clone())
            } else {
                None
            }
        })
        .collect();

    // call_1 must be present as both FunctionCall and FunctionCallOutput.
    assert!(
        function_calls.contains(&"call_1".to_string()),
        "completed FunctionCall call_1 must survive; got calls: {function_calls:?}"
    );
    assert!(
        function_outputs.contains(&"call_1".to_string()),
        "completed FunctionCallOutput call_1 must survive; got outputs: {function_outputs:?}"
    );

    // Orphaned call_2 must NOT be present.
    assert!(
        !function_calls.contains(&"call_2".to_string()),
        "orphaned FunctionCall call_2 must be removed"
    );

    // No Reasoning items (reasoning was stripped).
    let has_reasoning = input_items
        .iter()
        .any(|item| matches!(item, rs::InputItem::Item(rs::Item::Reasoning(_))));
    assert!(!has_reasoning, "reasoning items must be stripped");

    // Temperature must be absent.
    assert!(
        json.get("temperature").is_none()
            || json.pointer("/temperature").is_some_and(|v| v.is_null()),
        "temperature must be absent; got: {json:#}",
    );
}

#[test]
fn test_transform_cwd_rewrites_reasoning_sibling() {
    // Reasoning lives as a sibling now and IS subject to CWD rewriting
    // via `transform_conversation_cwd` (see the `Reasoning(_)` arm),
    // which is a behavior improvement over the pre-refactor state
    // where it lived buried in AssistantItem.reasoning and was skipped.
    let worktree = "/workspace/.echo-agent/worktrees/project/ab-uuid-a";
    let root = "/workspace/project";

    let mut items = vec![
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "rs_1".to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: format!("thinking about {worktree}"),
            })],
            content: None,
            encrypted_content: None,
            status: None,
        }),
        ConversationItem::Assistant(AssistantItem {
            content: format!("I edited {worktree}/src/main.rs").into(),
            tool_calls: vec![],
            model_id: Some("echo-agent-3".to_string()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ];

    transform_conversation_cwd(&mut items, worktree, root);

    assert_eq!(
        items[1].text_content(),
        format!("I edited {root}/src/main.rs")
    );
    let ConversationItem::Reasoning(r) = &items[0] else {
        panic!("expected Reasoning sibling");
    };
    let rs::SummaryPart::SummaryText(t) = &r.summary[0];
    assert!(
        !t.text.contains(worktree),
        "reasoning sibling text should be rewritten"
    );
    assert!(t.text.contains(root));
}

// ── Tool result with images tests ──────────────────────────────────────────

#[test]
fn test_tool_result_with_images_to_responses_api() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("System"),
        ConversationItem::user("Read this image"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"target_file": "photo.png"}"#.into(),
        }]),
        ConversationItem::tool_result_with_images(
            "call_1",
            "Read image file: photo.png",
            vec![ContentPart::Image {
                url: "data:image/png;base64,iVBOR".into(),
            }],
        ),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();

    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let fco_items: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = item {
                Some(fco)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(fco_items.len(), 1);
    assert_eq!(fco_items[0].call_id, "call_1");

    // Should be Content variant, not Text
    let rs::FunctionCallOutput::Content(parts) = &fco_items[0].output else {
        panic!("Expected Content output with images, got Text");
    };
    assert_eq!(parts.len(), 2, "Expected text + 1 image");
    assert!(
        matches!(&parts[0], rs::InputContent::InputText(t) if t.text == "Read image file: photo.png")
    );
    assert!(
        matches!(&parts[1], rs::InputContent::InputImage(img) if img.image_url.as_deref() == Some("data:image/png;base64,iVBOR"))
    );
}

#[test]
fn test_tool_result_without_images_stays_text() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Run ls"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "bash".to_string(),
            arguments: r#"{"command": "ls"}"#.into(),
        }]),
        ConversationItem::tool_result("call_1", "file1.txt\nfile2.txt"),
    ]);

    let responses_req: rs::CreateResponse = (&req).into();
    let rs::InputParam::Items(items) = responses_req.input else {
        panic!("Expected Items input");
    };
    let fco = items
        .iter()
        .find_map(|item| {
            if let rs::InputItem::Item(rs::Item::FunctionCallOutput(fco)) = item {
                Some(fco)
            } else {
                None
            }
        })
        .unwrap();

    // Should still be Text variant when no images
    assert!(matches!(&fco.output, rs::FunctionCallOutput::Text(t) if t == "file1.txt\nfile2.txt"));
}

#[test]
fn responses_api_conversion_preserves_model_fingerprint() {
    use std::collections::HashMap;

    let mut metadata = HashMap::new();
    metadata.insert("system_fingerprint".into(), "fp_abc123".into());

    let response = rs::Response {
        background: None,
        billing: None,
        conversation: None,
        created_at: 0,
        completed_at: None,
        error: None,
        id: "resp_test".into(),
        incomplete_details: None,
        instructions: None,
        max_output_tokens: None,
        metadata: Some(metadata),
        model: "echo-agent-4.5".into(),
        object: "response".into(),
        output: vec![rs::OutputItem::Message(rs::OutputMessage {
            content: vec![rs::OutputMessageContent::OutputText(
                rs::OutputTextContent {
                    text: "hello".into(),
                    annotations: vec![],
                    logprobs: None,
                },
            )],
            id: "msg_test".into(),
            role: rs::AssistantRole::Assistant,
            status: rs::OutputStatus::Completed,
        })],
        parallel_tool_calls: None,
        previous_response_id: None,
        prompt: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
        reasoning: None,
        safety_identifier: None,
        service_tier: None,
        status: rs::Status::Completed,
        temperature: None,
        text: None,
        tool_choice: None,
        tools: None,
        top_logprobs: None,
        top_p: None,
        truncation: None,
        usage: None,
    };

    let items = response_to_conversation_items(response);
    let item = items
        .into_iter()
        .next_back()
        .expect("response produces at least a trailing Assistant");
    assert_matches!(item, ConversationItem::Assistant(ref a) => {
        assert_eq!(a.model_fingerprint.as_deref(), Some("fp_abc123"));
        assert_eq!(a.model_id.as_deref(), Some("echo-agent-4.5"));
        assert_eq!(a.content.as_ref(), "hello");
    });
}

#[test]
fn empty_reason_reasoning_only() {
    // A response with a Reasoning sibling but empty Assistant content
    // is classified as ReasoningOnly so the retry logic resamples.
    let response = ConversationResponse {
        items: vec![
            ConversationItem::Reasoning(rs::ReasoningItem {
                id: "r1".to_string(),
                summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text: "thinking but no text output".to_string(),
                })],
                content: None,
                encrypted_content: None,
                status: None,
            }),
            ConversationItem::Assistant(AssistantItem {
                content: String::new().into(),
                tool_calls: Vec::new(),
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            }),
        ],
        stop_reason: Some(StopReason::Stop),
        usage: None,
        cost_usd_ticks: None,
        message_chunks_emitted: 0,
        doom_loop_signals: Vec::new(),
        stop_message: None,
        message_id: None,
        raw_stop_reason: None,
        stop_sequence: None,
    };
    assert_eq!(
        response.empty_reason(),
        Some(crate::error::EmptyReason::ReasoningOnly)
    );
    assert!(response.is_empty());
}

#[test]
fn build_responses_input_preserves_multi_turn_ordering() {
    // 4-turn conversation where each assistant turn carries reasoning.
    // The wire-level item order must be
    //     [Sys, U1, R, A1, U2, R, A2, U3, R, A3, U4, R, A4, U5]
    // and NOT the buggy
    //     [Sys, U1, U2, U3, U4, U5, R, A1, R, A2, ...]
    // which would shift the cache prefix every turn.
    fn r(text: &str) -> ConversationItem {
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: text.to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: text.to_string(),
            })],
            content: None,
            encrypted_content: Some(format!("enc_{text}")),
            status: None,
        })
    }
    let items: Vec<ConversationItem> = vec![
        ConversationItem::system("you are helpful"),
        ConversationItem::user("u1"),
        r("r1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("u2"),
        r("r2"),
        ConversationItem::assistant("a2"),
        ConversationItem::user("u3"),
        r("r3"),
        ConversationItem::assistant("a3"),
        ConversationItem::user("u4"),
        r("r4"),
        ConversationItem::assistant("a4"),
        ConversationItem::user("u5"),
    ];

    let req = ConversationRequest::from_items(items);
    let input = super::responses::build_responses_input(&req);
    let rs::InputParam::Items(wire_items) = input else {
        panic!("expected Items input");
    };

    // Walk the wire items and verify the expected pattern.
    // Roles per wire item: System, User, Reasoning(role=Assistant),
    // Assistant, User, Reasoning, Assistant, ...
    let kinds: Vec<&'static str> = wire_items
        .iter()
        .map(|w| match w {
            rs::InputItem::EasyMessage(m) => match m.role {
                rs::Role::System => "Sys",
                rs::Role::User => "U",
                rs::Role::Assistant => "A",
                _ => "other",
            },
            rs::InputItem::Item(rs::Item::Reasoning(_)) => "R",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "Sys", "U", "R", "A", "U", "R", "A", "U", "R", "A", "U", "R", "A", "U",
        ],
        "multi-turn ordering must preserve interleaved Reasoning ↔ Assistant per turn"
    );
}

#[test]
fn upgrade_legacy_reasoning_singular_chat_completions_text_only() {
    // Chat-completions has only text (no encrypted, no id).
    let raw = serde_json::json!({
        "type": "assistant",
        "content": "answer",
        "reasoning": {"text": "step-by-step plain reasoning"}
    });
    let mut seen = std::collections::HashSet::new();
    let siblings = upgrade_legacy_reasoning(&raw, &mut seen);
    assert_eq!(siblings.len(), 1);
    let ConversationItem::Reasoning(r) = &siblings[0] else {
        panic!("expected Reasoning sibling");
    };
    assert_eq!(r.id, "");
    assert!(r.encrypted_content.is_none());
    let rs::SummaryPart::SummaryText(s) = &r.summary[0];
    assert_eq!(s.text, "step-by-step plain reasoning");
}

#[test]
fn upgrade_legacy_reasoning_v0_chat_request_message_shape() {
    // v0 on disk: top-level role + reasoning_content.
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "v0 answer",
        "reasoning_content": "v0-style plain text reasoning"
    });
    let mut seen = std::collections::HashSet::new();
    let siblings = upgrade_legacy_reasoning(&raw, &mut seen);
    assert_eq!(siblings.len(), 1);
    let ConversationItem::Reasoning(r) = &siblings[0] else {
        panic!("expected Reasoning sibling");
    };
    let rs::SummaryPart::SummaryText(s) = &r.summary[0];
    assert_eq!(s.text, "v0-style plain text reasoning");
}

#[test]
fn patch_reasoning_text_types_injects_type_discriminator() {
    // Build a request body containing a reasoning item whose nested
    // `content[]` entries lack the `type` field (the async-openai gap).
    let mut body = serde_json::json!({
        "input": [
            {
                "type": "reasoning",
                "id": "r1",
                "content": [
                    { "text": "thinking..." },
                    { "text": "more thinking" }
                ]
            },
            {
                "type": "message",
                "role": "user",
                "content": "hi"
            }
        ]
    });
    patch_reasoning_text_types(&mut body);
    let reasoning_content = body
        .pointer("/input/0/content")
        .and_then(|v| v.as_array())
        .expect("reasoning content array");
    for item in reasoning_content {
        assert_eq!(
            item.get("type").and_then(|t| t.as_str()),
            Some("reasoning_text"),
            "every nested content item must carry the discriminator"
        );
    }
    // Untouched: the user message stays as-is.
    assert_eq!(
        body.pointer("/input/1/content").and_then(|v| v.as_str()),
        Some("hi")
    );
}

#[test]
fn patch_reasoning_text_types_preserves_existing_type() {
    let mut body = serde_json::json!({
        "input": [
            {
                "type": "reasoning",
                "id": "r1",
                "content": [
                    // Post-upstream-fix shape: discriminator already present.
                    { "type": "reasoning_text", "text": "already tagged" },
                    // A hypothetical different discriminator must NOT be clobbered.
                    { "type": "some_future_variant", "text": "future shape" },
                    // Current gap: missing type → gets filled in.
                    { "text": "needs tag" }
                ]
            }
        ]
    });
    patch_reasoning_text_types(&mut body);
    let content = body
        .pointer("/input/0/content")
        .and_then(|v| v.as_array())
        .expect("reasoning content array");

    // Existing discriminators preserved verbatim (no clobber).
    assert_eq!(
        content[0].get("type").and_then(|t| t.as_str()),
        Some("reasoning_text"),
    );
    assert_eq!(
        content[1].get("type").and_then(|t| t.as_str()),
        Some("some_future_variant"),
        "a non-default upstream discriminator must be left untouched",
    );
    // Only the type-less item is filled in.
    assert_eq!(
        content[2].get("type").and_then(|t| t.as_str()),
        Some("reasoning_text"),
    );

    // Object integrity: each item has exactly one `type` and its `text`.
    for item in content {
        let obj = item.as_object().expect("content item is an object");
        assert!(obj.contains_key("type") && obj.contains_key("text"));
    }
}

#[test]
fn build_responses_input_single_reasoning_sibling_lands_inline() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("u1"),
        reasoning_sibling("r_abc", "thinking", Some("enc1")),
        ConversationItem::assistant("hi"),
    ]);

    let input = input_items_json(&req);
    let summary = summarise_input(&input);

    // Expected: [system, user, reasoning, assistant]
    assert_eq!(summary.len(), 4, "got: {summary:?}");
    assert_eq!(summary[0], "system:sys");
    assert_eq!(summary[1], "user:u1");
    assert_eq!(summary[2], "reasoning:r_abc");
    assert_eq!(summary[3], "assistant:hi");

    // No placeholder strings must appear (post-refactor invariant).
    let body_str = serde_json::to_string(&input).unwrap();
    assert!(
        !body_str.contains("__RAW_OUTPUT_PLACEHOLDER_"),
        "no placeholder strings post-refactor"
    );

    // The reasoning item must carry encrypted_content verbatim.
    assert_eq!(
        input[2].get("encrypted_content").and_then(|v| v.as_str()),
        Some("enc1"),
    );
}

#[test]
fn build_responses_input_multi_turn_reasoning_ordering() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("u1"),
        reasoning_sibling("r1", "think 1", Some("enc1")),
        ConversationItem::assistant("a1"),
        ConversationItem::tool_result("tc1", "result1"),
        ConversationItem::user("u2"),
        reasoning_sibling("r2", "think 2", Some("enc2")),
        ConversationItem::assistant("a2"),
        ConversationItem::tool_result("tc2", "result2"),
        ConversationItem::user("u3"),
        reasoning_sibling("r3", "think 3", Some("enc3")),
        ConversationItem::assistant("a3"),
    ]);

    let input = input_items_json(&req);
    let summary = summarise_input(&input);

    // INVARIANT 1: There must be exactly N reasoning items for N
    // siblings. The pre-refactor bug produced only 1.
    let reasoning_count = summary
        .iter()
        .filter(|s| s.starts_with("reasoning:"))
        .count();
    assert_eq!(
        reasoning_count, 3,
        "must have 3 reasoning items, got {reasoning_count}. Items: {summary:?}"
    );

    // INVARIANT 2: Each reasoning must be BETWEEN its corresponding
    // user message and the NEXT user message. Without this check,
    // all reasoning items bunched at the end would still pass count.
    let user_positions: Vec<usize> = summary
        .iter()
        .enumerate()
        .filter(|(_, s)| s.starts_with("user:"))
        .map(|(i, _)| i)
        .collect();
    let reasoning_positions: Vec<usize> = summary
        .iter()
        .enumerate()
        .filter(|(_, s)| s.starts_with("reasoning:"))
        .map(|(i, _)| i)
        .collect();

    assert_eq!(user_positions.len(), 3);
    assert_eq!(reasoning_positions.len(), 3);

    for (i, rp) in reasoning_positions.iter().enumerate() {
        assert!(
            *rp > user_positions[i],
            "reasoning {i} at position {rp} must be after user {i} at position {}. \
                 Items: {summary:?}",
            user_positions[i]
        );
        if i + 1 < user_positions.len() {
            assert!(
                *rp < user_positions[i + 1],
                "reasoning {i} at position {rp} must be before user {} at position {}. \
                     Items: {summary:?}",
                i + 1,
                user_positions[i + 1]
            );
        }
    }

    // INVARIANT 3: encrypted_content per item is preserved 1:1.
    let mut enc_seen: Vec<&str> = Vec::new();
    for v in &input {
        if v.get("type").and_then(|t| t.as_str()) == Some("reasoning")
            && let Some(enc) = v.get("encrypted_content").and_then(|s| s.as_str())
        {
            enc_seen.push(enc);
        }
    }
    assert_eq!(enc_seen, vec!["enc1", "enc2", "enc3"]);
}

#[test]
fn backend_tool_call_position_stable() {
    let ws_a = ConversationItem::BackendToolCall(BackendToolCallItem {
        kind: BackendToolKind::WebSearch(rs::WebSearchToolCall {
            id: "ws_a".to_string(),
            status: rs::WebSearchToolCallStatus::Completed,
            action: rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                query: "alpha".to_string(),
                sources: Some(vec![]),
            }),
        }),
    });
    let ws_b = ConversationItem::BackendToolCall(BackendToolCallItem {
        kind: BackendToolKind::WebSearch(rs::WebSearchToolCall {
            id: "ws_b".to_string(),
            status: rs::WebSearchToolCallStatus::Completed,
            action: rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                query: "beta".to_string(),
                sources: Some(vec![]),
            }),
        }),
    });

    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("u1"),
        reasoning_sibling("r1", "think a", Some("enc_a")),
        ws_a,
        ConversationItem::assistant("a1"),
        ConversationItem::user("u2"),
        ws_b,
        ConversationItem::assistant("a2"),
    ]);

    let input = input_items_json(&req);
    let ws_items: Vec<&serde_json::Value> = input
        .iter()
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("web_search_call"))
        .collect();

    // Both backend tool calls must survive serialization.
    assert_eq!(
        ws_items.len(),
        2,
        "both web_search_call items must survive; got: {:?}",
        summarise_input(&input)
    );
    let ids: Vec<&str> = ws_items
        .iter()
        .filter_map(|v| v.get("id").and_then(|i| i.as_str()))
        .collect();
    assert_eq!(ids, vec!["ws_a", "ws_b"], "ordering preserved");
}

#[test]
fn empty_content_assistant_with_tool_calls_and_reasoning() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("u1"),
        reasoning_sibling("r1", "must call a tool", Some("enc_pre_tool")),
        ConversationItem::Assistant(AssistantItem {
            content: Arc::<str>::from(""),
            tool_calls: vec![ToolCall {
                id: Arc::<str>::from("call_1"),
                name: "read_file".to_string(),
                arguments: Arc::<str>::from("{}"),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("call_1", "file contents"),
    ]);

    let input = input_items_json(&req);
    let summary = summarise_input(&input);

    // Expected:
    //   user:u1
    //   reasoning:r1
    //   (assistant message DROPPED because content is empty -- per
    //    conversation_item_to_input_items, lines 1718-1724)
    //   function_call:call_1
    //   function_call_output (tool result)
    //
    // No spurious extra reasoning items, no placeholder.
    let reasoning_count = summary
        .iter()
        .filter(|s| s.starts_with("reasoning:"))
        .count();
    assert_eq!(
        reasoning_count, 1,
        "exactly one reasoning item; got: {summary:?}"
    );
    assert!(
        summary.iter().any(|s| s == "function_call:call_1"),
        "function_call must appear; got: {summary:?}"
    );
    assert!(
        summary
            .iter()
            .any(|s| s.starts_with("type:function_call_output")),
        "function_call_output must appear; got: {summary:?}"
    );

    let body_str = serde_json::to_string(&input).unwrap();
    assert!(!body_str.contains("__RAW_OUTPUT_PLACEHOLDER_"));
}

#[test]
fn serialized_body_contains_no_placeholder_strings() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("u1"),
        reasoning_sibling("r1", "first", Some("enc1")),
        ConversationItem::assistant("a1"),
        ConversationItem::user("u2"),
        reasoning_sibling("r2", "second", Some("enc2")),
        ConversationItem::assistant("a2"),
        ConversationItem::user("u3"),
    ]);

    let cr: rs::CreateResponse = (&req).into();
    let mut body = serde_json::to_value(&cr).unwrap();
    patch_reasoning_text_types(&mut body);
    let body_str = serde_json::to_string(&body).unwrap();

    assert!(
        !body_str.contains("__RAW_OUTPUT_PLACEHOLDER_"),
        "no placeholder strings post-sibling-Reasoning refactor"
    );

    // Both reasoning items must appear inline in the input array.
    let input = body["input"].as_array().unwrap();
    let reasoning_items: Vec<&serde_json::Value> = input
        .iter()
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("reasoning"))
        .collect();
    assert_eq!(
        reasoning_items.len(),
        2,
        "both reasoning siblings must be present"
    );
}
