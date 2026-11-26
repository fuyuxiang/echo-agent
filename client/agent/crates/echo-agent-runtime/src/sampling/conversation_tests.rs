use super::fork_filter_chat;
use echo_agent_sampling_types::conversation::ConversationItem;

#[test]
fn fork_filter_removes_synthetic_user_messages() {
    use echo_agent_sampling_types::conversation::*;

    let mut items = vec![
        ConversationItem::system("system prompt"),
        ConversationItem::user("real question"),
        ConversationItem::User(UserItem {
            content: vec![ContentPart::Text {
                text: "doom loop".into(),
            }],
            synthetic_reason: Some(SyntheticReason::SystemReminder),
            ..Default::default()
        }),
        ConversationItem::assistant("response"),
    ];
    fork_filter_chat(&mut items);
    assert!(
        !items.iter().any(|i| match i {
            ConversationItem::User(u) => u.synthetic_reason.is_some(),
            _ => false,
        }),
        "synthetic messages should be stripped"
    );
}
#[test]
fn fork_filter_truncates_at_complete_turn() {
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("q1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("q2"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(items.len(), 3, "should truncate after last complete turn");
    assert!(matches!(items[2], ConversationItem::Assistant(_)));
}
#[test]
fn fork_filter_consecutive_users_with_tool_calls() {
    use echo_agent_sampling_types::conversation::*;

    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("prefix"),
        ConversationItem::user("query"),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "output"),
        ConversationItem::user("follow-up"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        5,
        "should keep through complete tool turn, drop incomplete follow-up"
    );
}
#[test]
fn fork_filter_preserves_complete_tool_turn() {
    use echo_agent_sampling_types::conversation::*;

    let mut items = vec![
        ConversationItem::user("q"),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "output"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(items.len(), 3, "complete tool turn should be preserved");
}
#[test]
fn fork_filter_strips_incomplete_tool_turn() {
    use echo_agent_sampling_types::conversation::*;

    let mut items = vec![
        ConversationItem::user("q1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("q2"),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        2,
        "should truncate before incomplete tool turn (trailing user(q2) also dropped)"
    );
    assert!(matches!(items[0], ConversationItem::User(_)));
    assert!(matches!(items[1], ConversationItem::Assistant(_)));
}
#[test]
fn fork_filter_keeps_turn_with_reasoning_between_user_and_assistant() {
    use echo_agent_sampling_types::conversation::*;

    // Reasoning between the user query and the assistant must not end the
    // turn scan.
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("q"),
        ConversationItem::Reasoning(echo_agent_sampling_types::synthesized_reasoning_item(
            "thinking",
        )),
        ConversationItem::assistant("a"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        4,
        "reasoning between user and assistant must not truncate the turn: got {items:?}"
    );
    assert!(matches!(items[3], ConversationItem::Assistant(_)));
}
#[test]
fn fork_filter_keeps_multi_tool_turn_with_reasoning_between_results() {
    use echo_agent_sampling_types::conversation::*;

    // Reasoning between the tool results must not hide the second result
    // from the completeness scan.
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("q"),
        ConversationItem::Reasoning(echo_agent_sampling_types::synthesized_reasoning_item(
            "plan",
        )),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![
                ToolCall {
                    id: "tc1".into(),
                    name: "bash".into(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "tc2".into(),
                    name: "grep".into(),
                    arguments: "{}".into(),
                },
            ],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "out1"),
        ConversationItem::Reasoning(echo_agent_sampling_types::synthesized_reasoning_item("mid")),
        ConversationItem::tool_result("tc2", "out2"),
        ConversationItem::Reasoning(echo_agent_sampling_types::synthesized_reasoning_item(
            "reflect",
        )),
        ConversationItem::assistant("final"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        9,
        "multi-tool turn with reasoning between results must be fully kept: got {items:?}"
    );
    match items.last() {
        Some(ConversationItem::Assistant(a)) => assert_eq!(a.content.as_ref(), "final"),
        other => panic!("expected final assistant text last, got {other:?}"),
    }
}
#[test]
fn fork_filter_drops_trailing_incomplete_goal_turn_after_reasoning() {
    use echo_agent_sampling_types::conversation::*;

    // The in-flight /goal turn is a trailing bare user with no assistant; it
    // must be dropped even though a Reasoning sibling precedes the prior
    // assistant.
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("q"),
        ConversationItem::Reasoning(echo_agent_sampling_types::synthesized_reasoning_item(
            "thinking",
        )),
        ConversationItem::assistant("a"),
        ConversationItem::user("/goal do the thing"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        4,
        "trailing bare /goal user turn must be dropped: got {items:?}"
    );
    match items.last() {
        Some(ConversationItem::Assistant(a)) => assert_eq!(a.content.as_ref(), "a"),
        other => panic!("expected trailing assistant, got {other:?}"),
    }
}
