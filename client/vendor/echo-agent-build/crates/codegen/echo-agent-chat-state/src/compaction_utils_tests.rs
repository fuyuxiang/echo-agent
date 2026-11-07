use super::*;
use echo_agent_sampling_types::SyntheticReason;
use echo_agent_sampling_types::{BackendToolCallItem, BackendToolKind, rs};
#[test]
fn summarization_prep_drops_backend_tool_calls() {
    let items = vec![
        ConversationItem::user("hi"),
        ConversationItem::BackendToolCall(BackendToolCallItem {
            kind: BackendToolKind::WebSearch(rs::WebSearchToolCall {
                id: "ws_res-uuid_call-uuid-1".to_string(),
                status: rs::WebSearchToolCallStatus::Completed,
                action: rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                    query: "weather".to_string(),
                    sources: None,
                }),
            }),
        }),
        ConversationItem::assistant("done"),
    ];
    let prepared = prepare_conversation_for_summarization(items);
    assert!(
        !prepared
            .iter()
            .any(|i| matches!(i, ConversationItem::BackendToolCall(_))),
        "provider-minted native items must not reach the summarizer request"
    );
    assert_eq!(prepared.len(), 2);
}
#[test]
fn compaction_attempt_serde_roundtrip_and_skips_none() {
    let attempt = CompactionAttempt {
        attempt: 2,
        outcome: "degenerate".to_string(),
        summary_chars: 47,
        summary: Some("Now I will summarize: I'll do X, then Y, then Z.".to_string()),
        error: None,
    };
    let json = serde_json::to_value(&attempt).unwrap();
    assert_eq!(json["attempt"], 2);
    assert_eq!(json["outcome"], "degenerate");
    assert_eq!(json["summary_chars"], 47);
    assert_eq!(
        json["summary"],
        "Now I will summarize: I'll do X, then Y, then Z."
    );
    assert!(json.get("error").is_none());
    let parsed: CompactionAttempt = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, attempt);
}
#[test]
fn compaction_attempt_defaults_optional_fields_for_old_artifacts() {
    let json = serde_json::json!({
        "attempt": 1,
        "outcome": "transient",
        "summary_chars": 0,
    });
    let parsed: CompactionAttempt = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.summary, None);
    assert_eq!(parsed.error, None);
}
#[test]
fn bound_captured_output_returns_short_text_whole() {
    let s = "Now I will do X, Y, Z.";
    assert_eq!(bound_captured_output(s, MAX_CAPTURED_SUMMARY_CHARS), s);
}
#[test]
fn bound_captured_output_keeps_head_and_tail_on_char_boundaries() {
    let s: String = "λ".repeat(100);
    let bounded = bound_captured_output(&s, 10);
    assert!(bounded.starts_with("λλλλλ"));
    assert!(bounded.ends_with("λλλλλ"));
    assert!(bounded.contains("[90 chars elided]"));
    assert_eq!(bounded.matches('λ').count(), 10);
}
#[test]
fn test_extract_user_query_with_tags() {
    let input = r#"<user_info>
OS Version: macos
Shell: /bin/bash
</user_info>

<user_query>
create a hello world file
</user_query>"#;
    assert_eq!(extract_user_query(input), "create a hello world file");
}
#[test]
fn test_extract_user_query_multiline() {
    let input = r#"<user_query>
fix the bug in
the login page
</user_query>"#;
    assert_eq!(extract_user_query(input), "fix the bug in\nthe login page");
}
#[test]
fn test_extract_user_query_fallback() {
    let input = r#"<user_info>
OS Version: macos
</user_info>

some plain text"#;
    assert_eq!(extract_user_query(input), "some plain text");
}
#[test]
fn test_extract_user_query_plain_text() {
    let input = "just a simple query";
    assert_eq!(extract_user_query(input), "just a simple query");
}
#[test]
fn test_extract_user_query_strips_system_reminder_inside_user_query() {
    let input = "<user_query>\n\
         <system-reminder>\n\
         This is a scheduled task execution (task t-1, every 5m, recurring).\n\
         </system-reminder>\n\
         \n\
         print free memory\n\
         </user_query>";
    assert_eq!(extract_user_query(input), "print free memory");
}
#[test]
fn test_extract_user_query_ignores_nested_rules_query_before_outer_query() {
    let input = "<user_info>os</user_info>\n\n\
         <rules><always_applied_workspace_rules>\
         <always_applied_workspace_rule name=\"AGENTS.md\">\
         <user_query>instruction text</user_query>\
         </always_applied_workspace_rule>\
         </always_applied_workspace_rules></rules>\n\n\
         <user_query>real request</user_query>";
    assert_eq!(extract_user_query(input), "real request");
}
#[test]
fn test_strip_fork_context_tag() {
    let input = "<fork-context>\nYou inherited context.\n</fork-context>\n\nreal content";
    assert_eq!(extract_user_query(input), "real content");
}
#[test]
fn test_strip_system_reminder_tag() {
    let input = "<system-reminder>\nFollow these instructions.\n</system-reminder>\n\nreal content";
    assert_eq!(extract_user_query(input), "real content");
}
#[test]
fn test_strip_agent_memory_tag() {
    let input = "<agent-memory>\nPrevious context.\n</agent-memory>\n\nreal content";
    assert_eq!(extract_user_query(input), "real content");
}
#[test]
fn test_strip_system_underscore_reminder_tag() {
    let input = "<system_reminder>\nReminder text.\n</system_reminder>\n\nreal content";
    assert_eq!(extract_user_query(input), "real content");
}
#[test]
fn test_strip_background_context_tag() {
    let input = "<background_context>\nBackground info.\n</background_context>\n\nreal content";
    assert_eq!(extract_user_query(input), "real content");
}
#[test]
fn test_strip_command_name_tag() {
    let input = "<command-name>execute-plan</command-name>\n\nreal content";
    assert_eq!(extract_user_query(input), "real content");
}
#[test]
fn test_strip_command_message_tag() {
    let input = "<command-message>/execute-plan</command-message>\n\nreal content";
    assert_eq!(extract_user_query(input), "real content");
}
#[test]
fn test_strip_command_args_tag() {
    let input = "<command-args>--dry-run</command-args>\n\nreal content";
    assert_eq!(extract_user_query(input), "real content");
}
#[test]
fn test_strip_multiple_system_tags_at_once() {
    let input = "\
<user_info>OS: linux</user_info>
<fork-context>Inherited.</fork-context>
<system-reminder>Instructions here.</system-reminder>
<agent-memory>Memory data.</agent-memory>

actual user question";
    assert_eq!(extract_user_query(input), "actual user question");
}
#[test]
fn test_strip_unclosed_tag_left_intact() {
    let input = "<fork-context>\nUnclosed tag with no end\n\nreal content";
    assert_eq!(extract_user_query(input), input.trim());
}
#[test]
fn test_strip_system_tags_preserves_existing_behavior() {
    let input = "<user_info>\nOS Version: macos\n</user_info>\n\
                 <project_layout>\nfiles\n</project_layout>\n\
                 <git_status>\nclean\n</git_status>\n\nplain text remains";
    assert_eq!(extract_user_query(input), "plain text remains");
}
#[test]
fn test_strip_duplicate_tags() {
    let input = "<fork-context>A</fork-context><fork-context>B</fork-context> leftover";
    assert_eq!(extract_user_query(input), "leftover");
}
#[test]
fn test_strip_tags_empty_content() {
    let input = "<fork-context></fork-context>";
    assert_eq!(extract_user_query(input), "");
}
#[test]
fn test_strip_close_tag_before_open_tag() {
    let input = "</fork-context>text<fork-context>content</fork-context>more";
    assert_eq!(extract_user_query(input), "</fork-context>textmore");
}
#[test]
fn test_strip_nested_different_tags() {
    let input = "<fork-context>outer<system-reminder>inner</system-reminder></fork-context>rest";
    assert_eq!(extract_user_query(input), "rest");
}
#[test]
fn test_strip_rules_envelope() {
    let input = "<user_info>os</user_info>\n\n\
         <rules>\nintro\n\n\n\
         <user_rules description=\"These are user rules.\">\n\
         <user_rule><user_query>instruction text</user_query></user_rule>\n\
         </user_rules>\n\
         </rules>";
    assert_eq!(extract_user_query(input), "");
    assert!(!is_real_user_turn(&ConversationItem::user(input)));
}
#[test]
fn test_extract_last_user_query() {
    let history = vec![ConversationItem::user(
        "<user_info>OS: macos</user_info>\n\n<user_query>\nfix the bug\n</user_query>",
    )];
    let result = extract_last_user_query(&history);
    assert_eq!(result, Some("fix the bug".to_string()));
}
#[test]
fn test_extract_last_user_query_no_user_message() {
    let history = vec![
        ConversationItem::system("system prompt"),
        ConversationItem::assistant("hello"),
    ];
    assert!(extract_last_user_query(&history).is_none());
}
#[test]
fn test_extract_last_user_query_finds_latest() {
    let history = vec![
        ConversationItem::user(
            "<user_info>OS: macos</user_info>\n\n<user_query>\nfirst task\n</user_query>",
        ),
        ConversationItem::assistant("done"),
        ConversationItem::user(
            "<user_info>OS: macos</user_info>\n\n<user_query>\nsecond task\n</user_query>",
        ),
    ];
    let result = extract_last_user_query(&history);
    assert_eq!(result, Some("second task".to_string()));
}
#[test]
fn test_extract_real_user_queries_plain_text() {
    let conv = vec![
        ConversationItem::user("fix the auth bug"),
        ConversationItem::assistant("done"),
        ConversationItem::user("add a test"),
    ];
    let queries = extract_real_user_queries(&conv);
    assert_eq!(queries, vec!["fix the auth bug", "add a test"]);
}
#[test]
fn test_extract_real_user_queries_strips_prefix_returns_query() {
    let first_turn = "<user_info>\nOS Version: macos\n</user_info>\n\
                      <project_layout>\nfiles\n</project_layout>\n\
                      <user_query>\nimplement feature X\n</user_query>";
    let conv = vec![
        ConversationItem::user(first_turn),
        ConversationItem::assistant("done"),
        ConversationItem::user("also add tests"),
    ];
    let queries = extract_real_user_queries(&conv);
    assert_eq!(queries, vec!["implement feature X", "also add tests"]);
}
#[test]
fn test_extract_real_user_queries_excludes_metadata_only() {
    let metadata_only =
        "<user_info>\nOS Version: macos\n</user_info>\n<project_layout>\nfiles\n</project_layout>";
    let conv = vec![
        ConversationItem::user(metadata_only),
        ConversationItem::assistant("hello"),
        ConversationItem::user("real question"),
    ];
    let queries = extract_real_user_queries(&conv);
    assert_eq!(
        queries,
        vec!["real question"],
        "metadata-only prefix must be excluded"
    );
}
#[test]
fn test_extract_real_user_queries_excludes_auto_continue() {
    let conv = vec![
        ConversationItem::user("<user_query>\n__auto_continue__\n</user_query>"),
        ConversationItem::assistant("continuing"),
        ConversationItem::user("real prompt"),
        ConversationItem::user("<user_query>\n__auto_continue__\n</user_query>"),
    ];
    let queries = extract_real_user_queries(&conv);
    assert_eq!(
        queries,
        vec!["real prompt"],
        "auto-continue sentinels must be excluded"
    );
}
#[test]
fn test_extract_real_user_queries_empty_conversation() {
    let queries = extract_real_user_queries(&[]);
    assert!(queries.is_empty());
}
#[test]
fn test_extract_real_user_queries_no_user_items() {
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::assistant("hello"),
    ];
    let queries = extract_real_user_queries(&conv);
    assert!(queries.is_empty());
}
/// The actual AUTO_CONTINUE_PROMPT text stored in the conversation after
/// auto-compaction must NOT be counted as a real user query.
#[test]
fn test_extract_real_user_queries_excludes_actual_auto_continue_prompt() {
    let conv = vec![
        ConversationItem::user(
            "<user_info>OS: macos</user_info>\n<user_query>\nreal task\n</user_query>",
        ),
        ConversationItem::assistant("done"),
        // This is what run_inline_auto_continue() pushes after compaction:
        ConversationItem::user(AUTO_CONTINUE_PROMPT),
        ConversationItem::assistant("continuing..."),
    ];
    let queries = extract_real_user_queries(&conv);
    assert_eq!(
        queries,
        vec!["real task"],
        "AUTO_CONTINUE_PROMPT stored in conversation must be excluded"
    );
}
#[test]
fn test_is_synthetic_empty() {
    assert!(is_synthetic_extracted_query(""));
}
#[test]
fn test_is_synthetic_sentinel() {
    assert!(is_synthetic_extracted_query("__auto_continue__"));
}
#[test]
fn test_is_synthetic_auto_continue_prompt() {
    assert!(
        is_synthetic_extracted_query(AUTO_CONTINUE_PROMPT),
        "the full AUTO_CONTINUE_PROMPT text must be synthetic"
    );
}
#[test]
fn test_is_synthetic_real_query_is_false() {
    assert!(!is_synthetic_extracted_query("fix the auth bug"));
    assert!(!is_synthetic_extracted_query("add tests"));
}
#[test]
fn test_extract_last_real_user_query_skips_auto_continue_prompt() {
    let conv = vec![
        ConversationItem::user(
            "<user_info>OS: macos</user_info>\n<user_query>\nimplement feature Y\n</user_query>",
        ),
        ConversationItem::assistant("done"),
        ConversationItem::user(AUTO_CONTINUE_PROMPT),
        ConversationItem::assistant("continuing..."),
    ];
    let result = extract_last_real_user_query(&conv);
    assert_eq!(
        result,
        Some("implement feature Y".to_string()),
        "must skip AUTO_CONTINUE_PROMPT and return previous real query"
    );
}
#[test]
fn test_extract_last_real_user_query_no_real_query() {
    let conv = vec![
        ConversationItem::user(AUTO_CONTINUE_PROMPT),
        ConversationItem::assistant("done"),
    ];
    assert!(extract_last_real_user_query(&conv).is_none());
}
#[test]
fn test_extract_last_real_user_query_normal_session() {
    let conv = vec![
        ConversationItem::user(
            "<user_info>OS: macos</user_info>\n<user_query>\nfirst task\n</user_query>",
        ),
        ConversationItem::assistant("done"),
        ConversationItem::user("<user_query>\nsecond task\n</user_query>"),
    ];
    assert_eq!(
        extract_last_real_user_query(&conv),
        Some("second task".to_string())
    );
}
#[test]
fn extract_messages_since_last_user_finds_assistant_and_tool() {
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("q1"),
        ConversationItem::assistant("a1"),
        ConversationItem::tool_result("c1", "long result data"),
        ConversationItem::assistant("a2"),
    ];
    let msgs = extract_messages_since_last_user(&conv);
    assert_eq!(msgs.len(), 3);
    if let ConversationItem::ToolResult(ref tr) = msgs[1] {
        assert_eq!(tr.content.as_ref(), "Tool call omitted...");
    } else {
        panic!("expected ToolResult");
    }
}
#[test]
fn extract_messages_since_last_user_stops_at_user() {
    let conv = vec![
        ConversationItem::user("q1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("q2"),
        ConversationItem::assistant("a2"),
    ];
    let msgs = extract_messages_since_last_user(&conv);
    assert_eq!(msgs.len(), 1);
}
#[test]
fn extract_messages_since_last_user_empty_conversation() {
    let conv: Vec<ConversationItem> = vec![];
    let msgs = extract_messages_since_last_user(&conv);
    assert!(msgs.is_empty());
}
#[test]
fn extract_messages_since_last_user_only_system() {
    let conv = vec![ConversationItem::system("sys")];
    let msgs = extract_messages_since_last_user(&conv);
    assert!(msgs.is_empty());
}
#[test]
fn extract_messages_since_last_user_ends_with_user() {
    let conv = vec![
        ConversationItem::user("q1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("q2"),
    ];
    let msgs = extract_messages_since_last_user(&conv);
    assert!(msgs.is_empty());
}
#[test]
fn is_real_user_turn_true_for_real_user() {
    let item = ConversationItem::user("<user_query>\nfix the auth bug\n</user_query>");
    assert!(is_real_user_turn(&item));
}
#[test]
fn is_real_user_turn_false_for_system_reminder() {
    let item = ConversationItem::system_reminder("⚠️ SYSTEM REMINDER");
    assert!(!is_real_user_turn(&item));
}
#[test]
fn is_real_user_turn_false_for_auto_continue() {
    let item = ConversationItem::user(AUTO_CONTINUE_PROMPT);
    assert!(!is_real_user_turn(&item));
    let item = ConversationItem::auto_continue(AUTO_CONTINUE_PROMPT);
    assert!(!is_real_user_turn(&item));
}
#[test]
fn is_real_user_turn_false_for_auto_recovery() {
    let item = ConversationItem::auto_recovery("Try the tool again");
    assert!(!is_real_user_turn(&item));
}
#[test]
fn is_real_user_turn_false_for_empty_bootstrap() {
    let item = ConversationItem::user("<user_info>OS: macos</user_info>");
    assert!(!is_real_user_turn(&item));
}
#[test]
fn is_real_user_turn_false_for_non_user_items() {
    assert!(!is_real_user_turn(&ConversationItem::system("sys")));
    assert!(!is_real_user_turn(&ConversationItem::assistant("hi")));
}
#[test]
fn is_real_user_turn_true_for_image_only_user() {
    let item = ConversationItem::user_with_parts(vec![ContentPart::Image {
        url: "data:image/png;base64,abc".into(),
    }]);
    assert!(
        is_real_user_turn(&item),
        "image-only user prompt must be a real user turn"
    );
}
#[test]
fn is_real_user_turn_true_for_image_plus_text_user() {
    let item = ConversationItem::user_with_parts(vec![
        ContentPart::Text {
            text: "<user_query>\nwhat is this?\n</user_query>".into(),
        },
        ContentPart::Image {
            url: "data:image/png;base64,abc".into(),
        },
    ]);
    assert!(is_real_user_turn(&item));
}
#[test]
fn is_real_user_turn_false_for_compaction_meta() {
    let item = ConversationItem::user_meta("Called the read_file tool...");
    assert!(
        !is_real_user_turn(&item),
        "user_meta (CompactionMeta) messages must not be real user turns"
    );
}
#[test]
fn extract_messages_since_last_real_user_anchors_on_image_only_user() {
    let conv = vec![
        ConversationItem::user("<user_query>\nold task\n</user_query>"),
        ConversationItem::assistant("old response"),
        ConversationItem::user_with_parts(vec![ContentPart::Image {
            url: "data:image/png;base64,screenshot".into(),
        }]),
        ConversationItem::assistant("I see the image"),
    ];
    let msgs = extract_messages_since_last_real_user(&conv);
    assert_eq!(
        msgs.len(),
        1,
        "only the assistant after the image-only user should be included"
    );
}
#[test]
fn extract_last_real_user_query_skips_system_reminder_by_metadata() {
    let conv = vec![
        ConversationItem::user("<user_query>\nimplement feature X\n</user_query>"),
        ConversationItem::assistant("working on it..."),
        ConversationItem::system_reminder("⚠️ SYSTEM REMINDER — stop repeating"),
        ConversationItem::assistant("ok, changing approach"),
    ];
    assert_eq!(
        extract_last_real_user_query(&conv),
        Some("implement feature X".to_string()),
    );
}
#[test]
fn extract_messages_since_last_real_user_ignores_synthetic_boundary() {
    use echo_agent_sampling_types::ToolCall;
    let conv = vec![
        ConversationItem::user("<user_query>\ndo stuff\n</user_query>"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_AAA".into(),
            name: "search_replace".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("call_AAA", "ok"),
        ConversationItem::system_reminder("⚠️ SYSTEM REMINDER"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_BBB".into(),
            name: "search_replace".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("call_BBB", "cancelled"),
    ];
    let msgs = extract_messages_since_last_real_user(&conv);
    assert_eq!(msgs.len(), 4, "both assistant/tool pairs must be included");
    let tool_ids: Vec<&str> = msgs
        .iter()
        .filter_map(|m| match m {
            ConversationItem::ToolResult(tr) => Some(tr.tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        tool_ids.contains(&"call_AAA"),
        "call_AAA must not be orphaned"
    );
    assert!(tool_ids.contains(&"call_BBB"));
}
#[test]
fn extract_messages_since_last_real_user_stops_at_real_user() {
    let conv = vec![
        ConversationItem::user("<user_query>\nfirst\n</user_query>"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("<user_query>\nsecond\n</user_query>"),
        ConversationItem::assistant("a2"),
    ];
    let msgs = extract_messages_since_last_real_user(&conv);
    assert_eq!(msgs.len(), 1);
}
#[test]
fn extract_messages_since_last_real_user_fallback_no_real_user() {
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::assistant("greeting"),
    ];
    let msgs = extract_messages_since_last_real_user(&conv);
    assert_eq!(msgs.len(), 1);
}
#[test]
fn agent_message_projection_uses_generic_label_and_preserves_image_only_payload() {
    let agent_message = ConversationItem::agent_message("");
    let ConversationItem::User(mut image_only) = agent_message else {
        panic!("agent_message must construct a user item");
    };
    image_only.content = vec![ContentPart::Image {
        url: "data:image/png;base64,abc".into(),
    }];
    let raw = ConversationItem::User(image_only);
    let prepared = ModelRequestHistory::from_raw(vec![raw]);
    let projected = prepared.into_items();
    let ConversationItem::User(user) = &projected[0] else {
        panic!("projected agent message must stay a user item");
    };
    assert!(matches!(
        user.content.as_slice(),
        [ContentPart::Text { text }, ContentPart::Image { url }]
            if text.as_ref() == AGENT_MESSAGE_MODEL_LABEL
                && url.as_ref() == "data:image/png;base64,abc"
    ));
}
fn agent_message_anchors(items: &[ConversationItem]) -> Vec<&ConversationItem> {
    items
        .iter()
        .filter(|item| {
            matches!(
                item,
                ConversationItem::User(user)
                    if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
            )
        })
        .collect()
}
#[tokio::test]
async fn agent_message_anchor_survives_two_compactions_with_raw_content_unchanged() {
    let raw_agent_message = "model-authored message from another agent";
    let conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("<user_query>human task</user_query>"),
        ConversationItem::assistant("before agent message"),
        ConversationItem::agent_message(raw_agent_message),
        ConversationItem::notification_drain("runtime wake"),
        ConversationItem::task_completed("task wake"),
        ConversationItem::assistant("working tail"),
        ConversationItem::tool_result("tc1", "tool output"),
    ];
    let raw_agent_message_bytes = serde_json::to_vec(&conversation[3]).unwrap();
    async fn compact_once(conversation: &[ConversationItem]) -> Vec<ConversationItem> {
        let state_context =
            CompactionStateContext::build(conversation, CompactionInputs::default())
                .await
                .for_compaction();
        assert!(state_context.recent_messages.is_empty());
        build_compacted_history(CompactedHistoryInput {
            system_message: ConversationItem::system("sys"),
            user_message_prefix: "prefix".into(),
            agents_md_reminder: None,
            state_context: &state_context,
            compaction_summary: "summary".into(),
            system_reminder: None,
            summary_before_recent: false,
            transcript_hint: None,
            summary_count: 1,
        })
    }
    let compacted_once = compact_once(&conversation).await;
    assert!(matches!(
        &compacted_once[2],
        ConversationItem::User(user)
            if user.synthetic_reason.is_none()
                && compacted_once[2].text_content()
                    == "<user_query>\nhuman task\n</user_query>"
    ));
    assert!(matches!(
        &compacted_once[3],
        ConversationItem::User(user)
            if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
                && compacted_once[3].text_content() == raw_agent_message
    ));
    let compacted_once_anchors = agent_message_anchors(&compacted_once);
    assert_eq!(compacted_once_anchors.len(), 1);
    assert_eq!(
        serde_json::to_vec(compacted_once_anchors[0]).unwrap(),
        raw_agent_message_bytes
    );
    let compacted_twice = compact_once(&compacted_once).await;
    assert!(matches!(
        &compacted_twice[2],
        ConversationItem::User(user)
            if user.synthetic_reason.is_none()
                && compacted_twice[2].text_content()
                    == "<user_query>\nhuman task\n</user_query>"
    ));
    assert!(matches!(
        &compacted_twice[3],
        ConversationItem::User(user)
            if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
                && compacted_twice[3].text_content() == raw_agent_message
    ));
    let compacted_twice_anchors = agent_message_anchors(&compacted_twice);
    assert_eq!(compacted_twice_anchors.len(), 1);
    assert_eq!(compacted_twice_anchors[0].text_content(), raw_agent_message);
    assert_eq!(
        serde_json::to_vec(compacted_twice_anchors[0]).unwrap(),
        raw_agent_message_bytes
    );
    assert!(!compacted_twice.iter().any(|item| {
        matches!(
            item,
            ConversationItem::Assistant(_) | ConversationItem::ToolResult(_)
        )
    }));
    assert!(!compacted_twice.iter().any(|item| {
        matches!(
            item,
            ConversationItem::User(user)
                if matches!(
                    user.synthetic_reason,
                    Some(SyntheticReason::NotificationDrain | SyntheticReason::TaskCompleted)
                )
        )
    }));
    assert_eq!(extract_real_user_queries(&conversation), vec!["human task"]);
    let agent_only = vec![
        ConversationItem::system("sys"),
        ConversationItem::agent_message(raw_agent_message),
    ];
    let agent_only_context =
        CompactionStateContext::build(&agent_only, CompactionInputs::default()).await;
    assert!(agent_only_context.last_user_query.is_none());
    let agent_only_compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "prefix".into(),
        agents_md_reminder: None,
        state_context: &agent_only_context,
        compaction_summary: "summary".into(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    assert_eq!(agent_message_anchors(&agent_only_compacted).len(), 1);
}
#[tokio::test]
async fn agent_message_before_latest_human_survives_compaction_exactly_once() {
    let raw_agent_message = "generic assignment from another agent";
    let conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::agent_message(raw_agent_message),
        ConversationItem::notification_drain("runtime wake"),
        ConversationItem::task_completed("task wake"),
        ConversationItem::user("<user_query>latest human query</user_query>"),
        ConversationItem::assistant("working tail"),
    ];
    let raw_agent_message_bytes = serde_json::to_vec(&conversation[1]).unwrap();
    let state_context = CompactionStateContext::build(&conversation, CompactionInputs::default())
        .await
        .for_compaction();
    assert_eq!(
        state_context.last_user_query.as_deref(),
        Some("latest human query")
    );
    assert!(state_context.recent_messages.is_empty());
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "prefix".into(),
        agents_md_reminder: None,
        state_context: &state_context,
        compaction_summary: "summary".into(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    assert!(matches!(
        &compacted[2],
        ConversationItem::User(user)
            if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
                && compacted[2].text_content() == raw_agent_message
    ));
    assert!(matches!(
        &compacted[3],
        ConversationItem::User(user)
            if user.synthetic_reason.is_none()
                && compacted[3].text_content()
                    == "<user_query>\nlatest human query\n</user_query>"
    ));
    let compacted_anchors = agent_message_anchors(&compacted);
    assert_eq!(compacted_anchors.len(), 1);
    assert_eq!(compacted_anchors[0].text_content(), raw_agent_message);
    assert_eq!(
        serde_json::to_vec(compacted_anchors[0]).unwrap(),
        raw_agent_message_bytes
    );
    assert!(!compacted.iter().any(|item| {
        matches!(
            item,
            ConversationItem::User(user)
                if matches!(
                    user.synthetic_reason,
                    Some(SyntheticReason::NotificationDrain | SyntheticReason::TaskCompleted)
                )
        )
    }));
    let human_only = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("<user_query>latest human query</user_query>"),
    ];
    let human_only_context =
        CompactionStateContext::build(&human_only, CompactionInputs::default()).await;
    assert!(human_only_context.agent_message_anchor.is_none());
    let human_only_compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "prefix".into(),
        agents_md_reminder: None,
        state_context: &human_only_context,
        compaction_summary: "summary".into(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    assert!(agent_message_anchors(&human_only_compacted).is_empty());
}
#[tokio::test]
async fn compaction_state_context_build_uses_real_user_and_real_tail() {
    use echo_agent_sampling_types::ToolCall;
    let conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::user(
            "<user_info>OS: macos</user_info>\n\n<user_query>\nfix the bug\n</user_query>",
        ),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_X".into(),
            name: "edit".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("call_X", "done"),
        ConversationItem::system_reminder("⚠️ SYSTEM REMINDER"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_Y".into(),
            name: "edit".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("call_Y", "cancelled"),
    ];
    let ctx = CompactionStateContext::build(&conversation, CompactionInputs::default()).await;
    assert_eq!(ctx.last_user_query, Some("fix the bug".to_string()));
    assert_eq!(
        ctx.recent_messages.len(),
        4,
        "both assistant/tool pairs must survive synthetic-user boundary"
    );
    let assistant_ids: std::collections::HashSet<String> = ctx
        .recent_messages
        .iter()
        .filter_map(|m| match m {
            ConversationItem::Assistant(a) => {
                Some(a.tool_calls.iter().map(|tc| tc.id.as_ref().to_owned()))
            }
            _ => None,
        })
        .flatten()
        .collect();
    for msg in &ctx.recent_messages {
        if let ConversationItem::ToolResult(tr) = msg {
            assert!(
                assistant_ids.contains(&tr.tool_call_id),
                "tool_result {} must have a matching assistant tool_call",
                tr.tool_call_id
            );
        }
    }
}
#[tokio::test]
async fn test_compaction_state_context_build() {
    let conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::user(
            "<user_info>OS: macos</user_info>\n\n<user_query>\nfix the bug\n</user_query>",
        ),
        ConversationItem::assistant("Looking at it..."),
        ConversationItem::tool_result("tc1", "file contents"),
    ];
    let mut edited = BTreeSet::new();
    edited.insert("src/main.rs".to_string());
    let running = vec![CompactionStateContext::task_summary(
        "abc".to_string(),
        "cargo test".to_string(),
        "running",
        Some("run_terminal_command".to_string()),
    )];
    let ctx = CompactionStateContext::build(
        &conversation,
        CompactionInputs {
            running_tasks: running,
            agent_edited_paths: edited,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ctx.last_user_query, Some("fix the bug".to_string()));
    assert_eq!(ctx.recent_messages.len(), 2);
    assert_eq!(ctx.agent_edited_paths, vec!["src/main.rs".to_string()]);
    assert_eq!(ctx.running_tasks.len(), 1);
    assert_eq!(ctx.running_tasks[0].command, "cargo test");
}
#[tokio::test]
async fn build_stores_running_subagents() {
    let conversation = vec![
        ConversationItem::user("<user_query>\ntask\n</user_query>"),
        ConversationItem::assistant("working"),
    ];
    let subagents = vec![RunningSubagentSummary {
        subagent_id: "sub-x".into(),
        subagent_type: "Explore".into(),
        description: "searching".into(),
        elapsed_ms: 10_000,
    }];
    let ctx = CompactionStateContext::build(
        &conversation,
        CompactionInputs {
            running_subagents: subagents,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ctx.running_subagents.len(), 1);
    assert_eq!(ctx.running_subagents[0].subagent_id, "sub-x");
    assert_eq!(ctx.running_subagents[0].subagent_type, "Explore");
    assert_eq!(ctx.running_subagents[0].description, "searching");
    assert_eq!(ctx.running_subagents[0].elapsed_ms, 10_000);
}
#[tokio::test]
async fn build_stores_and_for_compaction_preserves_todos() {
    let conversation = vec![
        ConversationItem::user("<user_query>\ntask\n</user_query>"),
        ConversationItem::assistant("working"),
    ];
    let todos = vec![
        TodoSummary {
            id: "1".into(),
            content: "do the thing".into(),
            status: TodoSummaryStatus::InProgress,
        },
        TodoSummary {
            id: "2".into(),
            content: "do the other thing".into(),
            status: TodoSummaryStatus::Pending,
        },
    ];
    let ctx = CompactionStateContext::build(
        &conversation,
        CompactionInputs {
            todos,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ctx.todos.len(), 2);
    assert_eq!(ctx.todos[0].id, "1");
    assert_eq!(ctx.todos[0].status, TodoSummaryStatus::InProgress);
    let compacted = ctx.for_compaction();
    assert!(compacted.recent_messages.is_empty());
    assert_eq!(
        compacted.todos.len(),
        2,
        "todos must survive for_compaction() like other live state"
    );
    assert_eq!(compacted.todos[1].content, "do the other thing");
}
/// The compaction view drops the working transcript (`recent_messages`)
/// while preserving the last real user query and all other live state.
/// Built from a sub-agent-shaped conversation (ONE real user turn followed
/// by assistant/tool turns) so the dropped tail is genuinely non-empty AND
/// contains tool results — i.e. this would NOT pass if `for_compaction` were
/// a no-op.
#[tokio::test]
async fn for_compaction_drops_recent_messages_preserves_query() {
    use echo_agent_sampling_types::ToolCall;
    let conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::user(
            "<user_info>OS: macos</user_info>\n\n<user_query>\nimplement feature X\n</user_query>",
        ),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "tc1".into(),
            name: "read_file".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("tc1", "a".repeat(5000).as_str()),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "tc2".into(),
            name: "search_replace".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("tc2", "ok"),
        ConversationItem::assistant("done"),
    ];
    let mut edited = BTreeSet::new();
    edited.insert("src/x.rs".to_string());
    let running = vec![CompactionStateContext::task_summary(
        "t1".to_string(),
        "cargo test".to_string(),
        "running",
        None,
    )];
    let full = CompactionStateContext::build(
        &conversation,
        CompactionInputs {
            running_tasks: running,
            agent_edited_paths: edited,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        full.recent_messages.len(),
        5,
        "sub-agent: everything since the one real user turn is retained pre-fix"
    );
    assert!(
        full.recent_messages
            .iter()
            .any(|m| matches!(m, ConversationItem::ToolResult(_))),
        "the retained tail must contain tool results for this test to be meaningful"
    );
    let compacted = full.for_compaction();
    assert!(
        compacted.recent_messages.is_empty(),
        "for_compaction must drop the entire working transcript"
    );
    assert!(compacted.agent_message_anchor.is_none());
    assert_eq!(
        compacted.last_user_query,
        Some("implement feature X".to_string())
    );
    assert_eq!(compacted.agent_edited_paths, vec!["src/x.rs".to_string()]);
    assert_eq!(compacted.running_tasks.len(), 1);
    assert_eq!(compacted.running_tasks[0].command, "cargo test");
    assert_eq!(full.recent_messages.len(), 5);
}
#[test]
fn degenerate_one_liner_rejected() {
    let raw = "[Called tools: read_file, grep] Explored the compaction code and ran checks.";
    assert!(is_degenerate_summary(raw));
}
#[test]
fn degenerate_band_upper_bound_rejected() {
    let raw = "x".repeat(264);
    assert!(is_degenerate_summary(&raw));
}
#[test]
fn healthy_summary_accepted() {
    let raw = format!(
        "<summary>\n{}\n</summary>",
        "1. Primary Request: fix the bug. ".repeat(40)
    );
    assert!(!is_degenerate_summary(&raw));
}
#[test]
fn floor_boundary_at_500_chars() {
    assert!(is_degenerate_summary(&"y".repeat(499)));
    assert!(!is_degenerate_summary(&"y".repeat(500)));
}
#[test]
fn analysis_wrapping_empty_summary_rejected() {
    let raw = format!(
        "<analysis>\n{}\n</analysis>\n\n<summary>\n</summary>",
        "Walking through the conversation chronologically. ".repeat(100)
    );
    assert!(is_degenerate_summary(&raw));
}
#[test]
fn empty_cleaned_summary_rejected() {
    assert!(is_degenerate_summary(
        "<analysis>\nonly scratchpad, unclosed"
    ));
}
#[test]
fn format_compact_summary_strips_analysis_keeps_summary() {
    let input = "<analysis>\nThinking about the problem...\n</analysis>\n\n<summary>\n1. Primary Request: Fix the bug\n</summary>";
    let result = format_compact_summary(input);
    assert!(!result.contains("Analysis:"));
    assert!(!result.contains("Thinking about the problem"));
    assert!(result.contains("Summary:\n1. Primary Request: Fix the bug"));
    assert!(!result.contains("<analysis>"));
    assert!(!result.contains("</analysis>"));
    assert!(!result.contains("<summary>"));
    assert!(!result.contains("</summary>"));
}
#[test]
fn format_compact_summary_no_tags_passthrough() {
    let input = "Just plain text summary.";
    assert_eq!(format_compact_summary(input), "Just plain text summary.");
}
#[test]
fn format_compact_summary_only_summary() {
    let input = "<summary>\n1. Request: Do something\n</summary>";
    let result = format_compact_summary(input);
    assert_eq!(result, "Summary:\n1. Request: Do something");
}
#[test]
fn format_compact_summary_collapses_blank_lines() {
    let input = "<analysis>\nThought\n</analysis>\n\n\n\n<summary>\nResult\n</summary>";
    let result = format_compact_summary(input);
    assert!(!result.contains("\n\n\n"));
}
#[test]
fn format_compact_summary_analysis_with_summary_references_stripped() {
    let input = "<analysis>\nI need to wrap my output in <summary> tags as instructed.\nLet me organize the sections.\n</analysis>\n\n<summary>\n1. Primary Request: Fix bug\n</summary>";
    let result = format_compact_summary(input);
    assert!(!result.contains("wrap my output in <summary> tags"));
    assert!(!result.contains("<analysis>"));
    assert!(result.contains("Summary:\n1. Primary Request: Fix bug"));
}
#[test]
fn format_compact_summary_unclosed_analysis_strips_remainder() {
    let input = "<analysis>\nPartial reasoning about the task...";
    let result = format_compact_summary(input);
    assert_eq!(result, "");
}
#[test]
fn format_compact_summary_only_analysis_stripped() {
    let input = "<analysis>\nJust reasoning, no summary.\n</analysis>";
    let result = format_compact_summary(input);
    assert_eq!(result, "");
}
fn assert_clean_summary(result: &str) {
    assert!(
        result.starts_with("Summary:\n1. Primary Request"),
        "lost real section 1: {result:?}"
    );
    assert!(
        result.contains("9. Optional Next Step"),
        "lost trailing section: {result:?}"
    );
    for needle in [
        "<analysis>",
        "</analysis>",
        "<summary>",
        "</summary>",
        "**Analysis",
        "SCRATCHPAD",
    ] {
        assert!(!result.contains(needle), "leaked {needle:?}: {result:?}");
    }
}
#[test]
fn format_compact_summary_analysis_mentions_tags() {
    let raw = "<analysis>\nSCRATCHPAD: I'll wrap reasoning in <analysis> tags and the result in a <summary> block.\n</analysis>\n\n<summary>\n1. Primary Request and Intent\n- real content\n9. Optional Next Step\n- real next\n</summary>";
    assert_clean_summary(&format_compact_summary(raw));
}
#[test]
fn format_compact_summary_analysis_nested_in_summary() {
    let raw = "<summary>\n<analysis>\nSCRATCHPAD chronological reasoning.\n</analysis>\n\n1. Primary Request and Intent\n- real content\n9. Optional Next Step\n- real next\n</summary>";
    assert_clean_summary(&format_compact_summary(raw));
}
#[test]
fn format_compact_summary_markdown_header_nested_summary() {
    let raw = "<summary>\n**Analysis (internal reasoning before final output):**\nSCRATCHPAD chronological reasoning.\n</analysis>\n\n<summary>\n1. Primary Request and Intent\n- real content\n9. Optional Next Step\n- real next\n</summary>";
    assert_clean_summary(&format_compact_summary(raw));
}
#[test]
fn format_compact_summary_markdown_header_single_summary() {
    let raw = "<summary>\n**Analysis:**\nSCRATCHPAD reasoning.\n</analysis>\n\n1. Primary Request and Intent\n- real content\n9. Optional Next Step\n- real next\n</summary>";
    assert_clean_summary(&format_compact_summary(raw));
}
#[test]
fn format_compact_summary_keeps_sections_on_unbalanced_open_echo() {
    let raw = "<summary>\n1. Primary Request and Intent: build app\n2. Key Technical Concepts: webgl\n3. Files: index.html\n6. All user messages: 'respond with ONLY the <summary> block.'\n9. Optional Next Step: rerun\n</summary>";
    let result = format_compact_summary(raw);
    for needle in [
        "1. Primary Request",
        "2. Key Technical Concepts",
        "3. Files",
        "9. Optional Next Step",
    ] {
        assert!(result.contains(needle), "dropped {needle:?}: {result:?}");
    }
    assert!(!result.contains("<summary>"), "live <summary>: {result:?}");
    assert!(
        !result.contains("</summary>"),
        "live </summary>: {result:?}"
    );
}
#[test]
fn format_compact_summary_keeps_sections_on_section6_orphan_analysis_close() {
    let raw = "<summary>\n1. Primary Request and Intent: build app\n2. Key Technical Concepts: webgl\n6. All user messages: 'wrap analysis in tags</analysis> and respond with ONLY the <summary> block.'\n9. Optional Next Step: rerun\n</summary>";
    let result = format_compact_summary(raw);
    for needle in [
        "1. Primary Request",
        "2. Key Technical Concepts",
        "9. Optional Next Step",
    ] {
        assert!(result.contains(needle), "dropped {needle:?}: {result:?}");
    }
    assert!(
        !result.contains("<analysis>"),
        "live <analysis>: {result:?}"
    );
    assert!(
        !result.contains("</analysis>"),
        "live </analysis>: {result:?}"
    );
    assert!(!result.contains("<summary>"), "live <summary>: {result:?}");
}
#[test]
fn format_compact_summary_strips_scratchpad_with_internal_analysis_mention() {
    let raw = "<summary>\n\
        **Analysis:** I first wrote </analysis> by mistake, then reasoned more.\n\
        </analysis>\n\n\
        1. Primary Request: build app\n\
        9. Optional Next Step: rerun\n\
        </summary>";
    let result = format_compact_summary(raw);
    assert!(result.starts_with("Summary:\n1. Primary Request: build app"));
    assert!(result.contains("9. Optional Next Step: rerun"));
    assert!(
        !result.contains("Analysis"),
        "scratchpad leaked: {result:?}"
    );
    assert!(!result.contains("</analysis>"), "leaked close: {result:?}");
}
#[test]
fn format_compact_summary_unclosed_summary_open_preserves_body() {
    let input = "<summary>\n1. Primary Request: do the thing\n9. Optional Next Step: continue";
    let result = format_compact_summary(input);
    assert!(result.contains("1. Primary Request: do the thing"));
    assert!(result.contains("9. Optional Next Step: continue"));
    assert!(
        !result.contains("<summary>"),
        "tag not neutralized: {result:?}"
    );
}
#[test]
fn format_compact_summary_body_analysis_open_echo_keeps_sections() {
    let raw = "<summary>\n\
        1. Primary Request and Intent: build app\n\
        2. Key Technical Concepts: webgl\n\
        6. All user messages: 'wrap your analysis in <analysis> tags and respond with ONLY the <summary> block.'\n\
        9. Optional Next Step: rerun\n\
        </summary>";
    let result = format_compact_summary(raw);
    assert!(
        result.starts_with("Summary:\n1. Primary Request and Intent: build app"),
        "section 1 / heading lost: {result:?}"
    );
    for needle in ["2. Key Technical Concepts", "9. Optional Next Step"] {
        assert!(result.contains(needle), "dropped {needle:?}: {result:?}");
    }
    assert!(
        !result.contains("<analysis>"),
        "live <analysis>: {result:?}"
    );
    assert!(!result.contains("<summary>"), "live <summary>: {result:?}");
}
#[test]
fn format_compact_summary_nested_scratchpad_with_later_close_echo_keeps_sections() {
    let raw = "<summary>\n\
        <analysis>\nSCRATCHPAD reasoning.\n</analysis>\n\n\
        1. Primary Request: build app\n\
        6. All user messages: 'wrap analysis in tags</analysis> and respond'\n\
        9. Optional Next Step: rerun\n\
        </summary>";
    let result = format_compact_summary(raw);
    assert!(
        result.starts_with("Summary:\n1. Primary Request: build app"),
        "section 1 lost: {result:?}"
    );
    assert!(
        result.contains("9. Optional Next Step: rerun"),
        "section 9 lost: {result:?}"
    );
    assert!(
        !result.contains("SCRATCHPAD"),
        "scratchpad leaked: {result:?}"
    );
    assert!(
        !result.contains("</analysis>"),
        "live </analysis>: {result:?}"
    );
}
#[test]
fn format_compact_summary_body_analysis_pair_spanning_sections_keeps_them() {
    let raw = "<summary>\n\
        1. Primary Request: build app\n\
        6. All user messages: 'wrap your analysis in <analysis> tags'\n\
        7. Pending Tasks: fix the bug\n\
        8. Key files: foo.rs\n\
        9. Optional Next Step: 'end the block with </analysis> when done'\n\
        </summary>";
    let result = format_compact_summary(raw);
    for needle in [
        "1. Primary Request: build app",
        "7. Pending Tasks: fix the bug",
        "8. Key files: foo.rs",
        "9. Optional Next Step",
    ] {
        assert!(result.contains(needle), "dropped {needle:?}: {result:?}");
    }
    assert!(
        !result.contains("<analysis>"),
        "live <analysis>: {result:?}"
    );
    assert!(
        !result.contains("</analysis>"),
        "live </analysis>: {result:?}"
    );
}
#[test]
fn format_compact_summary_multiple_leading_analysis_blocks_all_stripped() {
    let raw = "<analysis>A reasoning</analysis>\n\
        <analysis>B reasoning</analysis>\n\
        <summary>\n1. Primary Request: build app\n9. Optional Next Step: rerun\n</summary>";
    let result = format_compact_summary(raw);
    assert!(
        result.starts_with("Summary:\n1. Primary Request: build app"),
        "scratchpad leaked ahead of heading: {result:?}"
    );
    assert!(result.contains("9. Optional Next Step: rerun"));
    assert!(
        !result.contains("reasoning"),
        "scratchpad prose leaked: {result:?}"
    );
    assert!(
        !result.contains("<analysis>"),
        "live <analysis>: {result:?}"
    );
}
#[test]
fn format_compact_summary_neutralizes_summary_request_tokens() {
    let raw = "1. Primary Request: build app\n\
        6. msgs: '<summary_request>do X</summary_request>'\n\
        9. Optional Next Step: rerun";
    let result = format_compact_summary(raw);
    assert!(
        !result.contains("<summary_request>"),
        "live <summary_request>: {result:?}"
    );
    assert!(
        !result.contains("</summary_request>"),
        "live </summary_request>: {result:?}"
    );
    assert!(result.contains("1. Primary Request: build app"));
    assert!(result.contains("9. Optional Next Step: rerun"));
}
#[test]
fn format_compact_summary_body_reversed_analysis_echo_not_garbled() {
    let raw = "<summary>\n\
        1. Primary Request: build app\n\
        6. msgs: 'output </analysis> then wrap in <analysis> tags'\n\
        9. Optional Next Step: rerun\n\
        </summary>";
    let result = format_compact_summary(raw);
    assert!(result.starts_with("Summary:\n1. Primary Request: build app"));
    assert!(result.contains("9. Optional Next Step: rerun"));
    assert_eq!(
        result.matches("then wrap in").count(),
        1,
        "spanned text duplicated: {result:?}"
    );
}
#[test]
fn format_compact_summary_markdown_numbered_lead_keeps_sections() {
    let raw = "<summary>\n\
        ## 1. Primary Request: build app\n\
        ## 6. All user messages: 'wrap analysis in tags</analysis> and respond.'\n\
        ## 9. Optional Next Step: rerun\n\
        </summary>";
    let result = format_compact_summary(raw);
    for needle in [
        "1. Primary Request: build app",
        "9. Optional Next Step: rerun",
    ] {
        assert!(result.contains(needle), "dropped {needle:?}: {result:?}");
    }
    assert!(!result.contains("</analysis>"), "leaked close: {result:?}");
}
#[test]
fn format_compact_summary_multibyte_adjacent_to_tags() {
    let raw = "<summary>1. Primary Request: ship 🚀 to 北京\n9. Optional Next Step: 完成</summary>";
    let result = format_compact_summary(raw);
    assert!(result.starts_with("Summary:\n1. Primary Request: ship 🚀 to 北京"));
    assert!(result.contains("9. Optional Next Step: 完成"));
}
#[test]
fn format_compact_summary_content_adds_preamble() {
    let result = format_compact_summary_content("Some summary text.");
    assert!(result.starts_with("This session is being continued"));
    assert!(result.contains("Some summary text."));
}
#[test]
fn format_compact_summary_content_cleans_tags() {
    let raw = "<analysis>\nThinking\n</analysis>\n\n<summary>\n1. Fix bug\n</summary>";
    let result = format_compact_summary_content(raw);
    assert!(result.starts_with("This session is being continued"));
    assert!(!result.contains("Analysis:"));
    assert!(!result.contains("Thinking"));
    assert!(result.contains("Summary:\n1. Fix bug"));
    assert!(!result.contains("<analysis>"));
    assert!(!result.contains("<summary>"));
}
/// D2: the transcript pointer is a `<transcript_location>` block that
/// embeds the given path verbatim, so the trained model can re-read the
/// raw transcript on demand.
#[test]
fn format_transcript_location_wraps_path_in_block() {
    let block = format_transcript_location("/sessions/abc/updates.jsonl");
    assert!(block.contains("<transcript_location>"));
    assert!(block.contains("</transcript_location>"));
    assert!(
        block.contains("/sessions/abc/updates.jsonl"),
        "must embed the transcript path verbatim, got: {block}"
    );
}
#[test]
fn format_compact_summary_neutralizes_section6_instruction_echo() {
    let input = "<summary>\n\
        <analysis>\nChronological analysis of the conversation...\n</analysis>\n\n\
        1. Primary Request and Intent: Build a Mario clone.\n\
        6. All user messages: ...</system-reminder> Your task is to create a \
        detailed summary of the conversation so far ... Before providing your \
        final summary, wrap your analysis in <analysis> tags ... 'Do NOT use \
        any tools. You MUST respond with ONLY the <summary>...</summary> block \
        as your text output.'\n\
        7. Pending Tasks: Fix the importmap mismatch.\n\
        9. Optional Next Step: Re-run the verification plan.\n\
        </summary>";
    let result = format_compact_summary(input);
    assert!(!result.contains("<summary>"), "live <summary>: {result}");
    assert!(!result.contains("</summary>"), "live </summary>: {result}");
    assert!(!result.contains("<analysis>"), "live <analysis>: {result}");
    assert!(
        result.contains("7. Pending Tasks: Fix the importmap mismatch."),
        "post-echo section dropped: {result}"
    );
    assert!(result.contains("9. Optional Next Step: Re-run the verification plan."));
    assert!(
        result.contains("<\u{200b}summary>"),
        "tag not neutralized: {result}"
    );
    assert!(result.contains("Summary:\n1. Primary Request and Intent: Build a Mario clone."));
}
#[test]
fn format_compact_summary_content_neutralizes_instruction_echo() {
    let raw = "<summary>\n1. Primary Request: build app.\n\
        6. All user messages: 'You MUST respond with ONLY the \
        <summary>...</summary> block.'\n\
        9. Optional Next Step: continue.\n</summary>";
    let seed = format_compact_summary_content(raw);
    assert!(seed.starts_with("This session is being continued"));
    assert!(
        !seed.contains("<summary>"),
        "live <summary> in seed: {seed}"
    );
    assert!(
        !seed.contains("</summary>"),
        "live </summary> in seed: {seed}"
    );
    assert!(seed.contains("9. Optional Next Step: continue."));
}
#[test]
fn format_compact_summary_malformed_tag_order_does_not_panic() {
    let input = "intro </summary> middle <summary> tail";
    let result = format_compact_summary(input);
    assert!(!result.contains("<summary>"));
    assert!(!result.contains("</summary>"));
    assert!(result.contains("intro"));
    assert!(result.contains("tail"));
}
#[test]
fn sanitize_strips_orphaned_tool_result() {
    use echo_agent_sampling_types::ToolCall;
    let items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("prompt"),
        // Orphaned tool result — no assistant with matching tool_calls
        ConversationItem::tool_result("call_ORPHAN", "result"),
        // Valid pair
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_VALID".into(),
            name: "read_file".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("call_VALID", "ok"),
    ];
    let result = sanitize_compacted_history(items);
    assert_eq!(result.stripped_tool_call_ids, vec!["call_ORPHAN"]);
    assert_eq!(result.items.len(), 4);
    for item in &result.items {
        if let ConversationItem::ToolResult(tr) = item {
            assert_eq!(tr.tool_call_id, "call_VALID");
        }
    }
}
#[test]
fn sanitize_keeps_assistant_with_unanswered_tool_calls() {
    use echo_agent_sampling_types::ToolCall;
    let items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("prompt"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_UNANSWERED".into(),
            name: "run_cmd".to_string(),
            arguments: "{}".into(),
        }]),
    ];
    let result = sanitize_compacted_history(items);
    assert!(result.stripped_tool_call_ids.is_empty());
    assert_eq!(result.items.len(), 3);
}
#[test]
fn sanitize_strips_result_before_call() {
    use echo_agent_sampling_types::ToolCall;
    let items = vec![
        ConversationItem::system("sys"),
        ConversationItem::tool_result("call_X", "premature result"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_X".into(),
            name: "read_file".to_string(),
            arguments: "{}".into(),
        }]),
    ];
    let result = sanitize_compacted_history(items);
    assert_eq!(
        result.stripped_tool_call_ids,
        vec!["call_X"],
        "result-before-call must be stripped"
    );
    assert_eq!(result.items.len(), 2);
}
#[test]
fn validate_detects_result_before_call() {
    use echo_agent_sampling_types::ToolCall;
    let items = vec![
        ConversationItem::tool_result("call_X", "premature"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_X".into(),
            name: "edit".to_string(),
            arguments: "{}".into(),
        }]),
    ];
    let invalid = validate_compacted_history(&items);
    assert_eq!(invalid, vec!["call_X"]);
}
#[test]
fn validate_passes_valid_history() {
    use echo_agent_sampling_types::ToolCall;
    let items = vec![
        ConversationItem::system("sys"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_A".into(),
            name: "edit".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("call_A", "done"),
    ];
    assert!(validate_compacted_history(&items).is_empty());
}
#[test]
fn sanitize_noop_on_valid_conversation() {
    use echo_agent_sampling_types::ToolCall;
    let items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("prompt"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_A".into(),
            name: "edit".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("call_A", "done"),
        ConversationItem::assistant("All done."),
    ];
    let result = sanitize_compacted_history(items);
    assert!(result.stripped_tool_call_ids.is_empty());
    assert_eq!(result.items.len(), 5);
}
fn call(id: &str) -> echo_agent_sampling_types::ToolCall {
    echo_agent_sampling_types::ToolCall {
        id: id.into(),
        name: "read_file".to_string(),
        arguments: "{}".into(),
    }
}
/// The bricked-session shape: the assistant line owning a batch of tool
/// calls was lost (torn/merged JSONL line skipped on load), so its
/// results are orphans. Repair must strip them and change nothing else.
#[test]
fn repair_history_strips_orphaned_tool_results() {
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("prompt"),
        // ← the assistant declaring call_LOST is missing here
        ConversationItem::tool_result("call_LOST", "orphaned result"),
        ConversationItem::assistant_tool_calls(vec![call("call_OK")]),
        ConversationItem::tool_result("call_OK", "fine"),
    ];
    let report = repair_history(&mut items);
    assert!(report.changed());
    assert_eq!(report.stripped_tool_result_ids, vec!["call_LOST"]);
    assert_eq!(report.duplicates_removed, 0);
    assert_eq!(report.synthetic_results_inserted, 0);
    assert_eq!(items.len(), 4);
}
/// A result displaced past a user turn has a matching id *somewhere
/// before*, so the compaction sanitizer would keep it — but providers
/// require adjacency, so repair must strip it and synthesize a result
/// for the now-unanswered call.
#[test]
fn repair_history_strips_displaced_result_and_backfills_call() {
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::assistant_tool_calls(vec![call("call_D")]),
        ConversationItem::user("interjection splits the pair"),
        ConversationItem::tool_result("call_D", "arrived too late"),
    ];
    let report = repair_history(&mut items);
    assert_eq!(report.stripped_tool_result_ids, vec!["call_D"]);
    assert_eq!(report.synthetic_results_inserted, 1);
    match (&items[1], &items[2]) {
        (ConversationItem::Assistant(a), ConversationItem::ToolResult(tr)) => {
            assert_eq!(a.tool_calls[0].id.as_ref(), "call_D");
            assert_eq!(tr.tool_call_id, "call_D");
            assert!(
                tr.content
                    .contains("halted by the harness (history_repair)"),
                "expected synthetic wording, got: {}",
                tr.content
            );
        }
        other => panic!("expected assistant+synthetic result, got {other:?}"),
    }
}
/// A result split from its owner by another assistant item is stripped
/// and the call backfilled — keeping it would make the dangling pass
/// insert a synthetic duplicate beside it (two results for one id).
#[test]
fn repair_history_strips_result_split_by_assistant_item() {
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::assistant_tool_calls(vec![call("call_A")]),
        ConversationItem::assistant("interleaved text"),
        ConversationItem::tool_result("call_A", "no longer contiguous"),
    ];
    let report = repair_history(&mut items);
    assert_eq!(report.stripped_tool_result_ids, vec!["call_A"]);
    assert_eq!(report.synthetic_results_inserted, 1);
    let results: Vec<_> = items
        .iter()
        .filter_map(|i| match i {
            ConversationItem::ToolResult(tr) => Some(tr),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    assert!(results[0].content.contains("halted by the harness"));
}
/// A result whose owner lives before an *earlier, separate* result run
/// must be stripped: the intervening run flushed the assistant message.
#[test]
fn repair_history_strips_result_in_later_run() {
    let mut items = vec![
        ConversationItem::assistant_tool_calls(vec![call("call_A"), call("call_B")]),
        ConversationItem::tool_result("call_A", "ok"),
        ConversationItem::assistant_tool_calls(vec![call("call_C")]),
        ConversationItem::tool_result("call_C", "ok"),
        // call_B's owner was flushed two messages ago.
        ConversationItem::tool_result("call_B", "displaced"),
    ];
    let report = repair_history(&mut items);
    assert_eq!(report.stripped_tool_result_ids, vec!["call_B"]);
    assert_eq!(report.synthetic_results_inserted, 1);
}
#[test]
fn repair_history_dedups_duplicate_results() {
    let mut items = vec![
        ConversationItem::assistant_tool_calls(vec![call("call_A")]),
        ConversationItem::tool_result("call_A", "stale duplicate"),
        ConversationItem::tool_result("call_A", "real result"),
    ];
    let report = repair_history(&mut items);
    assert_eq!(report.duplicates_removed, 1);
    assert!(report.stripped_tool_result_ids.is_empty());
    match &items[1] {
        ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.content.as_ref(), "real result")
        }
        other => panic!("expected tool result, got {other:?}"),
    }
}
#[test]
fn repair_history_is_noop_and_idempotent_on_valid_history() {
    let valid = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("prompt"),
        ConversationItem::assistant_tool_calls(vec![call("call_A")]),
        ConversationItem::tool_result("call_A", "done"),
        ConversationItem::assistant("All done."),
    ];
    let mut items = valid.clone();
    let report = repair_history(&mut items);
    assert!(!report.changed());
    assert_eq!(items.len(), valid.len());
    let mut corrupted = vec![
        ConversationItem::user("prompt"),
        ConversationItem::tool_result("call_ORPHAN", "orphan"),
    ];
    assert!(repair_history(&mut corrupted).changed());
    assert!(!repair_history(&mut corrupted).changed());
}
#[test]
fn wrap_user_query_wraps_text() {
    let result = wrap_user_query("hello world");
    assert_eq!(result, "<user_query>\nhello world\n</user_query>");
}
#[test]
fn wrap_user_query_preserves_multiline() {
    let result = wrap_user_query("line 1\nline 2");
    assert_eq!(result, "<user_query>\nline 1\nline 2\n</user_query>");
}
#[tokio::test]
async fn build_compacted_history_full_scenario() {
    let conversation = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user(
            "<user_info>OS: macos</user_info>\n\n<user_query>\nfix the login bug\n</user_query>",
        ),
        ConversationItem::assistant("Let me look."),
        ConversationItem::tool_result("tc1", "file contents here"),
        ConversationItem::assistant("Found the bug, fixing."),
    ];
    let mut edited = BTreeSet::new();
    edited.insert("src/auth.rs".to_string());
    let running_tasks = vec![BackgroundTaskSummary {
        task_id: "task1".into(),
        command: "cargo test".into(),
        status: "running".into(),
        tool_name: Some("run_terminal_command".into()),
    }];
    let state_context = CompactionStateContext::build(
        &conversation,
        CompactionInputs {
            running_tasks,
            agent_edited_paths: edited,
            ..Default::default()
        },
    )
    .await;
    let system_reminder =
        "<system-reminder>\n## Files Edited This Session\n- src/auth.rs\n</system-reminder>"
            .to_string();
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("You are a helpful assistant."),
        user_message_prefix: "<user_info>OS: macos</user_info>".to_string(),
        agents_md_reminder: None,
        state_context: &state_context,
        compaction_summary: "Summary: fixed login bug.".to_string(),
        system_reminder: Some(system_reminder.clone()),
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    assert_eq!(compacted.len(), 8);
    assert_eq!(compacted[0].text_content(), "You are a helpful assistant.");
    let prefix = compacted[1].text_content();
    assert_eq!(prefix, "<user_info>OS: macos</user_info>");
    assert!(!prefix.contains("<user_query>"));
    let query = compacted[2].text_content();
    assert_eq!(query, "<user_query>\nfix the login bug\n</user_query>");
    assert_eq!(compacted[3].text_content(), "Let me look.");
    assert_eq!(compacted[4].text_content(), "Tool call omitted...");
    assert_eq!(compacted[5].text_content(), "Found the bug, fixing.");
    let summary = compacted[6].text_content();
    assert!(
        !summary.contains("<user_query>"),
        "summary should NOT be wrapped in <user_query> tags"
    );
    assert!(
        summary.starts_with("This session is being continued"),
        "summary should start with the preamble"
    );
    assert!(summary.contains("Summary: fixed login bug."));
    assert!(
        !summary.contains("<system-reminder>"),
        "system-reminder should NOT be in the summary message"
    );
    let reminder = compacted[7].text_content();
    assert!(reminder.contains("<system-reminder>"));
    assert!(reminder.contains("Files Edited This Session"));
    if let ConversationItem::User(u) = &compacted[1] {
        assert_eq!(
            u.synthetic_reason,
            Some(SyntheticReason::CompactionMeta),
            "user_message_prefix should be tagged CompactionMeta"
        );
    }
    if let ConversationItem::User(u) = &compacted[6] {
        assert_eq!(
            u.synthetic_reason,
            Some(SyntheticReason::CompactionMeta),
            "compaction summary should be tagged CompactionMeta"
        );
    }
    if let ConversationItem::User(u) = &compacted[7] {
        assert_eq!(
            u.synthetic_reason,
            Some(SyntheticReason::SystemReminder),
            "system-reminder should be tagged SystemReminder"
        );
    }
}
#[tokio::test]
async fn build_compacted_history_minimal_no_reminder() {
    let conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("<user_query>\nhello\n</user_query>"),
        ConversationItem::assistant("Hi!"),
    ];
    let state_context =
        CompactionStateContext::build(&conversation, CompactionInputs::default()).await;
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "<user_info>OS: linux</user_info>".to_string(),
        agents_md_reminder: None,
        state_context: &state_context,
        compaction_summary: "Summary: user said hello.".to_string(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    assert_eq!(compacted.len(), 5);
    let summary = compacted[4].text_content();
    assert!(
        summary.starts_with("This session is being continued"),
        "summary should start with preamble (no <user_query> wrapping)"
    );
    assert!(
        summary.contains("Summary: user said hello."),
        "summary should contain the original summary text"
    );
    assert!(
        !summary.contains("<user_query>"),
        "summary should NOT contain <user_query> tags"
    );
    assert!(!summary.contains("<system-reminder>"));
}
#[tokio::test]
async fn build_compacted_history_no_user_query() {
    let conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::assistant("proactive greeting"),
    ];
    let state_context =
        CompactionStateContext::build(&conversation, CompactionInputs::default()).await;
    assert!(state_context.last_user_query.is_none());
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "prefix".to_string(),
        agents_md_reminder: None,
        state_context: &state_context,
        compaction_summary: "Summary".to_string(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    assert_eq!(compacted.len(), 4);
    assert_eq!(compacted[2].text_content(), "proactive greeting");
}
#[tokio::test]
async fn build_compacted_history_transcript_hint() {
    let conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("<user_query>\nfix the bug\n</user_query>"),
        ConversationItem::assistant("Fixed it."),
    ];
    let state_context =
        CompactionStateContext::build(&conversation, CompactionInputs::default()).await;
    let input = |path: Option<String>| CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "prefix".to_string(),
        agents_md_reminder: None,
        state_context: &state_context,
        compaction_summary: "Summary of work.".to_string(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: crate::CompactionMode::Transcript.transcript_hint(path.as_deref()),
        summary_count: 1,
    };
    let summary = build_compacted_history(input(Some("/sessions/abc/updates.jsonl".to_string())))
        .last()
        .unwrap()
        .text_content();
    assert!(summary.contains("/sessions/abc/updates.jsonl"));
    let summary = build_compacted_history(input(None))
        .last()
        .unwrap()
        .text_content();
    assert!(!summary.contains("transcript"));
}
/// Full multi-turn conversation with parallel tool calls, then compaction.
///
/// Simulates the exact conversation shape produced by echo-agent-runtime:
///
/// Turn 1: user_query → assistant(2 tool calls) → 2 tool results
/// Turn 2: user_query → assistant(2 tool calls) → 2 tool results
/// → compaction fires
///
/// Verifies the exact structure and content of the compacted output,
/// including how `<user_query>` tags appear and how tool calls/results
/// are preserved or omitted.
#[tokio::test]
async fn build_compacted_history_multi_turn_with_parallel_tool_calls() {
    use echo_agent_sampling_types::{AssistantItem, ToolCall};
    let conversation = vec![
        // [0] System prompt
        ConversationItem::system("You are a helpful coding assistant."),
        // [1] User info prefix (no <user_query> tags — this is the initial message)
        ConversationItem::user(
            "<user_info>\nOS Version: macos\nShell: /bin/bash\nWorkspace Path: /Users/dev/project\n</user_info>\n\n<project_layout>\n/Users/dev/project/\n  src/\n    main.rs\n    lib.rs\n</project_layout>",
        ),
        // ── Turn 1 ──────────────────────────────────────────────────
        // [2] User query (wrapped in <user_query> tags by parse_prompt)
        ConversationItem::user(
            "<user_query>\nRead main.rs and lib.rs and tell me what they do\n</user_query>",
        ),
        // [3] Assistant with 2 parallel tool calls
        ConversationItem::Assistant(AssistantItem {
            content: "I'll read both files for you.".into(),
            tool_calls: vec![
                ToolCall {
                    id: "call_1".into(),
                    name: "read_file".to_string(),
                    arguments: r#"{"target_file":"src/main.rs"}"#.into(),
                },
                ToolCall {
                    id: "call_2".into(),
                    name: "read_file".to_string(),
                    arguments: r#"{"target_file":"src/lib.rs"}"#.into(),
                },
            ],
            model_id: Some("echo-agent-3".to_string()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        // [4] Tool result for call_1
        ConversationItem::tool_result("call_1", "fn main() {\n    println!(\"hello world\");\n}"),
        // [5] Tool result for call_2
        ConversationItem::tool_result(
            "call_2",
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}",
        ),
        // [6] Assistant summary after reading both files
        ConversationItem::assistant("main.rs prints hello world. lib.rs has an `add` function."),
        // ── Turn 2 ──────────────────────────────────────────────────
        // [7] User query (second turn)
        ConversationItem::user(
            "<user_query>\nNow fix the typo in main.rs and run the tests\n</user_query>",
        ),
        // [8] Assistant with 2 parallel tool calls
        ConversationItem::Assistant(AssistantItem {
            content: "I'll fix the typo and run tests.".into(),
            tool_calls: vec![
                ToolCall {
                    id: "call_3".into(),
                    name: "edit_file".to_string(),
                    arguments: r#"{"target_file":"src/main.rs","new_string":"Hello, world!"}"#
                        .into(),
                },
                ToolCall {
                    id: "call_4".into(),
                    name: "run_terminal_cmd".to_string(),
                    arguments: r#"{"command":"cargo test"}"#.into(),
                },
            ],
            model_id: Some("echo-agent-3".to_string()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        // [9] Tool result for call_3
        ConversationItem::tool_result("call_3", "File edited successfully."),
        // [10] Tool result for call_4
        ConversationItem::tool_result(
            "call_4",
            "running 1 test\ntest tests::test_add ... ok\n\ntest result: ok. 1 passed",
        ),
        // [11] Assistant final response
        ConversationItem::assistant("Fixed the typo and all tests pass!"),
    ];
    let mut edited = BTreeSet::new();
    edited.insert("src/main.rs".to_string());
    let state_context = CompactionStateContext::build(
        &conversation,
        CompactionInputs {
            agent_edited_paths: edited,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        state_context.last_user_query,
        Some("Now fix the typo in main.rs and run the tests".to_string()),
        "should extract the last user query (turn 2)"
    );
    assert_eq!(
        state_context.recent_messages.len(),
        4,
        "should have 4 recent messages (assistant + 2 tool results + assistant)"
    );
    let system_reminder =
        "<system-reminder>\n## Files Edited\n- src/main.rs\n</system-reminder>".to_string();
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("You are a helpful coding assistant."),
        user_message_prefix: "<user_info>\nOS Version: macos\nShell: /bin/bash\nWorkspace Path: /Users/dev/project\n</user_info>"
            .to_string(),
        agents_md_reminder: None,
        state_context: &state_context,
        compaction_summary: "The user asked to read main.rs and lib.rs. main.rs prints hello world, lib.rs has an add function. The user then asked to fix a typo in main.rs and run tests. The typo was fixed and tests passed."
            .to_string(),
        system_reminder: Some(system_reminder),
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    assert_eq!(compacted.len(), 9, "compacted history should have 9 items");
    assert!(
        matches!(&compacted[0], ConversationItem::System(s) if s.content.as_ref() == "You are a helpful coding assistant.")
    );
    let prefix = compacted[1].text_content();
    assert!(
        prefix.contains("<user_info>"),
        "item[1] should be the user_info prefix"
    );
    assert!(
        !prefix.contains("<user_query>"),
        "item[1] prefix should NOT have <user_query> tags"
    );
    let last_query = compacted[2].text_content();
    assert_eq!(
        last_query, "<user_query>\nNow fix the typo in main.rs and run the tests\n</user_query>",
        "item[2] should be the last user query wrapped in <user_query> tags"
    );
    match &compacted[3] {
        ConversationItem::Assistant(a) => {
            assert_eq!(a.content.as_ref(), "I'll fix the typo and run tests.");
            assert_eq!(a.tool_calls.len(), 2, "should preserve both tool calls");
            assert_eq!(a.tool_calls[0].name, "edit_file");
            assert_eq!(a.tool_calls[1].name, "run_terminal_cmd");
        }
        other => panic!("item[3] should be Assistant, got {:?}", other),
    }
    match &compacted[4] {
        ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "call_3");
            assert_eq!(
                tr.content.as_ref(),
                "Tool call omitted...",
                "tool result content should be replaced with placeholder"
            );
        }
        other => panic!("item[4] should be ToolResult, got {:?}", other),
    }
    match &compacted[5] {
        ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "call_4");
            assert_eq!(tr.content.as_ref(), "Tool call omitted...");
        }
        other => panic!("item[5] should be ToolResult, got {:?}", other),
    }
    assert_eq!(
        compacted[6].text_content(),
        "Fixed the typo and all tests pass!"
    );
    let summary = compacted[7].text_content();
    assert!(
        !summary.contains("<user_query>"),
        "summary should NOT be wrapped in <user_query> tags"
    );
    assert!(
        summary.contains("The user asked to read main.rs"),
        "summary should contain the compaction text"
    );
    assert!(
        !summary.contains("<system-reminder>"),
        "system-reminder should NOT be in the summary message"
    );
    assert!(
        summary.starts_with("This session is being continued from a previous conversation"),
        "summary should start with the continuation preamble"
    );
    let expected_summary = "\
This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.

The user asked to read main.rs and lib.rs. main.rs prints hello world, lib.rs has an add function. The user then asked to fix a typo in main.rs and run tests. The typo was fixed and tests passed.";
    assert_eq!(
        summary, expected_summary,
        "summary item should match expected format with preamble"
    );
    let reminder = compacted[8].text_content();
    assert!(
        reminder.contains("<system-reminder>"),
        "reminder message should contain <system-reminder>"
    );
    assert!(
        reminder.contains("## Files Edited"),
        "system-reminder should contain files edited"
    );
    assert!(
        reminder.contains("src/main.rs"),
        "system-reminder should list edited files"
    );
}
/// Generation zero ignores relocation-only fields and preserves legacy output.
#[test]
fn generation_zero_compaction_keeps_legacy_project_instructions() {
    let state_context = CompactionStateContext {
        knowledge_context: None,
        cwd_generation: 0,
        destination_project_instructions: Some("destination rules".into()),
        agent_message_anchor: None,
        recent_messages: vec![],
        last_user_query: None,
        agent_edited_paths: vec![],
        running_tasks: vec![],
        running_subagents: vec![],
        connected_mcp_servers: vec![],
        todos: vec![],
    };
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "prefix".into(),
        agents_md_reminder: Some("startup rules".into()),
        state_context: &state_context,
        compaction_summary: "summary".into(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    assert_eq!(compacted[2].text_content(), "startup rules");
}
#[test]
fn relocated_compaction_uses_destination_project_instructions() {
    let state_context = CompactionStateContext {
        knowledge_context: None,
        cwd_generation: 1,
        destination_project_instructions: Some("destination rules".into()),
        agent_message_anchor: None,
        recent_messages: vec![],
        last_user_query: None,
        agent_edited_paths: vec![],
        running_tasks: vec![],
        running_subagents: vec![],
        connected_mcp_servers: vec![],
        todos: vec![],
    };
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "prefix".into(),
        agents_md_reminder: Some("startup rules".into()),
        state_context: &state_context,
        compaction_summary: "summary".into(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    assert_eq!(compacted[2].text_content(), "destination rules");
}
#[test]
fn relocated_compaction_does_not_restore_source_instructions_when_destination_has_none() {
    let state_context = CompactionStateContext {
        knowledge_context: None,
        cwd_generation: 1,
        destination_project_instructions: None,
        agent_message_anchor: None,
        recent_messages: vec![],
        last_user_query: None,
        agent_edited_paths: vec![],
        running_tasks: vec![],
        running_subagents: vec![],
        connected_mcp_servers: vec![],
        todos: vec![],
    };
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "prefix".into(),
        agents_md_reminder: Some("source rules".into()),
        state_context: &state_context,
        compaction_summary: "summary".into(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    assert!(!compacted.iter().any(|item| {
        matches!(item, ConversationItem::User(user) if user.synthetic_reason == Some(SyntheticReason::ProjectInstructions))
    }));
}
/// The AGENTS.md slot must use the structural project-instructions tag.
#[test]
fn build_compacted_history_tags_agents_md_with_project_instructions() {
    let state_context = CompactionStateContext {
        knowledge_context: None,
        cwd_generation: 0,
        destination_project_instructions: None,
        agent_message_anchor: None,
        recent_messages: vec![],
        last_user_query: None,
        agent_edited_paths: vec![],
        running_tasks: vec![],
        running_subagents: vec![],
        connected_mcp_servers: vec![],
        todos: vec![],
    };
    let reminder = "some AGENTS.md body".to_string();
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "<user_info>OS: macos</user_info>".to_string(),
        agents_md_reminder: Some(reminder.clone()),
        state_context: &state_context,
        compaction_summary: "Summary body.".to_string(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    let ConversationItem::User(u) = &compacted[2] else {
        panic!("compacted[2] should be the AGENTS.md User slot");
    };
    assert_eq!(
        u.synthetic_reason,
        Some(SyntheticReason::ProjectInstructions),
        "AGENTS.md slot must be tagged ProjectInstructions so the \
         spawn-time idempotence guard skips re-insertion on resume \
         from the compacted jsonl"
    );
    assert_eq!(
        compacted[2].text_content(),
        reminder,
        "AGENTS.md slot must carry the reminder text verbatim"
    );
}
/// When `agents_md_reminder` is `None`, no `ProjectInstructions`-tagged
/// item is emitted in the compacted history.
#[test]
fn build_compacted_history_omits_agents_md_when_none() {
    let state_context = CompactionStateContext {
        knowledge_context: None,
        cwd_generation: 0,
        destination_project_instructions: None,
        agent_message_anchor: None,
        recent_messages: vec![],
        last_user_query: None,
        agent_edited_paths: vec![],
        running_tasks: vec![],
        running_subagents: vec![],
        connected_mcp_servers: vec![],
        todos: vec![],
    };
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: "<user_info>OS: macos</user_info>".to_string(),
        agents_md_reminder: None,
        state_context: &state_context,
        compaction_summary: "Summary body.".to_string(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    let has_project_instructions = compacted.iter().any(|item| {
        matches!(
            item,
            ConversationItem::User(u)
                if u.synthetic_reason == Some(SyntheticReason::ProjectInstructions)
        )
    });
    assert!(
        !has_project_instructions,
        "no ProjectInstructions-tagged item should appear when \
         agents_md_reminder is None"
    );
}
#[test]
fn conversation_item_drops_tool_results() {
    let result = strip_tool_messages_for_conversation_item(vec![
        ConversationItem::system("system"),
        ConversationItem::user("hello"),
        ConversationItem::assistant("response"),
        ConversationItem::tool_result("call_1", "result"),
    ]);
    assert_eq!(result.len(), 3);
    assert!(
        !result
            .iter()
            .any(|m| matches!(m, ConversationItem::ToolResult(_)))
    );
}
/// Load-bearing: documents the intentional contract that
/// `strip_tool_messages_for_conversation_item` does NOT touch sibling
/// `Reasoning` items. `prepare_conversation_for_summarization` composes
/// against this guarantee by chaining `strip_reasoning_blocks` after.
#[test]
fn conversation_item_preserves_reasoning_siblings() {
    use echo_agent_sampling_types::{AssistantItem, rs};
    let result = strip_tool_messages_for_conversation_item(vec![
        ConversationItem::system("system"),
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "r_123".to_string(),
            summary: vec![],
            content: None,
            encrypted_content: Some("encrypted_sig".to_string()),
            status: None,
        }),
        ConversationItem::Assistant(AssistantItem {
            content: "response".into(),
            tool_calls: vec![],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ]);
    assert_eq!(result.len(), 3);
    assert!(matches!(result[1], ConversationItem::Reasoning(_)));
}
#[test]
fn strip_reasoning_blocks_drops_reasoning_siblings() {
    use echo_agent_sampling_types::{AssistantItem, rs};
    let result = strip_reasoning_blocks(vec![
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "r_123".to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: "thinking".to_string(),
            })],
            content: None,
            encrypted_content: Some("encrypted_sig".to_string()),
            status: None,
        }),
        ConversationItem::Assistant(AssistantItem {
            content: "response".into(),
            tool_calls: vec![],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ]);
    assert_eq!(result.len(), 1, "reasoning sibling must be dropped");
    assert!(matches!(result[0], ConversationItem::Assistant(_)));
}
#[test]
fn strip_reasoning_blocks_passes_other_items_through() {
    let result = strip_reasoning_blocks(vec![
        ConversationItem::system("system"),
        ConversationItem::user("hello"),
        ConversationItem::tool_result("call_1", "result"),
    ]);
    assert_eq!(result.len(), 3);
    assert!(matches!(result[0], ConversationItem::System(_)));
    assert!(matches!(result[1], ConversationItem::User(_)));
    assert!(matches!(result[2], ConversationItem::ToolResult(_)));
}
/// Reproduces the production failure that prompted this helper: an
/// assistant turn with both signed `reasoning` and `tool_calls` triggers a
/// provider "thinking blocks cannot be modified" 400 because the strip
/// mutates the surrounding text. After `prepare_conversation_for_summarization`
/// the message must have no `reasoning` left for the provider to validate.
#[test]
fn prepare_for_summarization_drops_reasoning_sibling_on_mutated_assistant() {
    use echo_agent_sampling_types::{AssistantItem, ToolCall, rs};
    let mk_reasoning = || {
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "r_123".to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: "plan".to_string(),
            })],
            content: None,
            encrypted_content: Some("encrypted_sig".to_string()),
            status: None,
        })
    };
    let result = prepare_conversation_for_summarization(vec![
        ConversationItem::system("system"),
        ConversationItem::user("do stuff"),
        mk_reasoning(),
        ConversationItem::Assistant(AssistantItem {
            content: "I'll search.".into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "grep".into(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "match found"),
    ]);
    assert_eq!(
        result.len(),
        3,
        "tool_result and reasoning sibling must be dropped"
    );
    assert!(
        !result
            .iter()
            .any(|m| matches!(m, ConversationItem::Reasoning(_))),
        "reasoning sibling must be dropped"
    );
    let ConversationItem::Assistant(a) = &result[2] else {
        panic!("expected assistant at index 2");
    };
    assert!(a.tool_calls.is_empty(), "tool_calls must be cleared");
    assert!(
        a.content.contains("[Called tools: grep]"),
        "tool annotation must be appended; got {:?}",
        a.content,
    );
}
#[test]
fn prepare_for_summarization_drops_standalone_reasoning_sibling() {
    use echo_agent_sampling_types::{AssistantItem, rs};
    let result = prepare_conversation_for_summarization(vec![
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "r_123".to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: "thinking".to_string(),
            })],
            content: None,
            encrypted_content: None,
            status: None,
        }),
        ConversationItem::Assistant(AssistantItem {
            content: "plain text response".into(),
            tool_calls: vec![],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ]);
    assert_eq!(result.len(), 1);
    let ConversationItem::Assistant(a) = &result[0] else {
        panic!("expected assistant");
    };
    assert_eq!(a.content.as_ref(), "plain text response");
}
/// Multi-assistant conversation with mixed reasoning/tool_calls states.
#[test]
fn prepare_for_summarization_handles_multi_assistant_mixed_conversation() {
    use echo_agent_sampling_types::{AssistantItem, ToolCall, rs};
    let mk_reasoning = || {
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "r".to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: "thinking".to_string(),
            })],
            content: None,
            encrypted_content: Some("sig".to_string()),
            status: None,
        })
    };
    let result = prepare_conversation_for_summarization(vec![
        ConversationItem::user("first turn"),
        mk_reasoning(),
        ConversationItem::Assistant(AssistantItem {
            content: "calling grep".into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "grep".into(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "match"),
        ConversationItem::user("second turn"),
        mk_reasoning(),
        ConversationItem::Assistant(AssistantItem {
            content: "thinking only".into(),
            tool_calls: vec![],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc2", "stray"),
        ConversationItem::user("third turn"),
        ConversationItem::Assistant(AssistantItem {
            content: "plain reply".into(),
            tool_calls: vec![],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ]);
    assert_eq!(result.len(), 6);
    assert!(
        !result
            .iter()
            .any(|m| matches!(m, ConversationItem::ToolResult(_)))
    );
    assert!(
        !result
            .iter()
            .any(|m| matches!(m, ConversationItem::Reasoning(_)))
    );
    let assistants: Vec<&AssistantItem> = result
        .iter()
        .filter_map(|m| match m {
            ConversationItem::Assistant(a) => Some(a),
            _ => None,
        })
        .collect();
    assert_eq!(assistants.len(), 3);
    for a in &assistants {
        assert!(a.tool_calls.is_empty(), "tool_calls must be cleared");
    }
    assert!(
        assistants[0].content.contains("[Called tools: grep]"),
        "tool-calling assistant must get annotation; got {:?}",
        assistants[0].content
    );
    assert!(
        !assistants[1].content.contains("[Called tools:"),
        "no-tool-call assistant must not get annotation; got {:?}",
        assistants[1].content
    );
    assert!(
        !assistants[2].content.contains("[Called tools:"),
        "plain assistant must not get annotation; got {:?}",
        assistants[2].content
    );
}
/// Calling `prepare_conversation_for_summarization` twice must produce
/// the same result as calling it once. Guarantees the transformation
/// has no hidden state and is safe to apply defensively at multiple
/// layers (e.g. memory flush + compaction both routing through it).
#[test]
fn prepare_for_summarization_is_idempotent() {
    use echo_agent_sampling_types::{AssistantItem, ToolCall, rs};
    let input = vec![
        ConversationItem::system("system prompt"),
        ConversationItem::user("hello"),
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "r1".to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: "thought".to_string(),
            })],
            content: None,
            encrypted_content: Some("sig".to_string()),
            status: None,
        }),
        ConversationItem::Assistant(AssistantItem {
            content: "hi".into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "ls".into(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "files"),
    ];
    let once = prepare_conversation_for_summarization(input.clone());
    let twice = prepare_conversation_for_summarization(once.clone());
    let once_json = serde_json::to_value(&once).unwrap();
    let twice_json = serde_json::to_value(&twice).unwrap();
    assert_eq!(once_json, twice_json, "second pass must be a no-op");
}
#[test]
fn test_strip_images_replaces_with_placeholder() {
    let mut user = ConversationItem::user("describe this");
    user.add_image("data:image/png;base64,iVBORw0KGgo=");
    let input = vec![
        ConversationItem::system("sys"),
        user,
        ConversationItem::assistant("I see an image"),
    ];
    let result = strip_images(input);
    match &result[1] {
        ConversationItem::User(u) => {
            assert_eq!(u.content.len(), 2);
            match &u.content[1] {
                ContentPart::Text { text } => assert_eq!(text.as_ref(), "[image]"),
                ContentPart::Image { .. } => panic!("image should have been stripped"),
            }
        }
        _ => panic!("expected User item"),
    }
}
#[test]
fn test_strip_images_leaves_text_only_messages_unchanged() {
    let input = vec![
        ConversationItem::user("just text"),
        ConversationItem::assistant("reply"),
    ];
    let result = strip_images(input);
    assert_eq!(result[0].text_content(), "just text");
}
#[test]
fn test_prepare_for_summarization_strips_images() {
    let mut user = ConversationItem::user("look at this");
    user.add_image("data:image/jpeg;base64,/9j/4AAQ");
    let input = vec![
        ConversationItem::system("sys"),
        user,
        ConversationItem::assistant("ok"),
    ];
    let result = prepare_conversation_for_summarization(input);
    match &result[1] {
        ConversationItem::User(u) => {
            for part in &u.content {
                assert!(
                    !matches!(part, ContentPart::Image { .. }),
                    "images should be stripped by prepare_conversation_for_summarization"
                );
            }
        }
        _ => panic!("expected User item"),
    }
}
/// The segment view must KEEP verbatim tool I/O (calls + results) — that's
/// what lets the model recover exact outputs — while the summary view drops
/// it. Guards against anyone collapsing the two preps into one.
#[test]
fn prepare_conversation_for_segment_keeps_tool_io_unlike_summary() {
    use echo_agent_sampling_types::ToolCall;
    let mut user = ConversationItem::user("read a.rs");
    user.add_image("data:image/png;base64,iVBORw0KGgo=");
    let conv = vec![
        ConversationItem::system("sys"),
        user,
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "c1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"target_file":"a.rs"}"#.into(),
        }]),
        ConversationItem::tool_result("c1", "fn main() {}"),
    ];
    let has_tool_calls = |items: &[ConversationItem]| {
        items
            .iter()
            .any(|i| matches!(i, ConversationItem::Assistant(a) if !a.tool_calls.is_empty()))
    };
    let has_tool_result = |items: &[ConversationItem]| {
        items
            .iter()
            .any(|i| matches!(i, ConversationItem::ToolResult(_)))
    };
    let has_image = |items: &[ConversationItem]| {
        items.iter().any(|i| {
            matches!(i, ConversationItem::User(u)
                if u.content.iter().any(|p| matches!(p, ContentPart::Image { .. })))
        })
    };
    let seg = prepare_conversation_for_segment(conv.clone());
    assert!(
        has_tool_calls(&seg),
        "segment view must keep structured tool calls"
    );
    assert!(has_tool_result(&seg), "segment view must keep tool results");
    assert!(!has_image(&seg), "segment view must strip base64 images");
    let summ = prepare_conversation_for_summarization(conv);
    assert!(!has_tool_calls(&summ), "summary view flattens tool calls");
    assert!(!has_tool_result(&summ), "summary view drops tool results");
}
/// Verbatim view keeps tool calls (with arguments) and results — no flattening, no dropped results.
#[test]
fn verbatim_keeps_tool_calls_args_and_results() {
    use echo_agent_sampling_types::ToolCall;
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("read a.rs"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "c1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"target_file":"a.rs"}"#.into(),
        }]),
        ConversationItem::tool_result("c1", "fn main() {}"),
    ];
    let result = prepare_conversation_for_verbatim_summarization(conv, false);
    match &result[2] {
        ConversationItem::Assistant(a) => {
            assert_eq!(a.tool_calls.len(), 1, "tool call must survive verbatim");
            assert_eq!(a.tool_calls[0].name, "read_file");
            assert!(
                a.tool_calls[0].arguments.contains("a.rs"),
                "arguments (the path) must be preserved, not dropped"
            );
            assert!(
                !a.content.contains("[Called tools:"),
                "verbatim view must NOT flatten tool calls into text"
            );
        }
        _ => panic!("expected Assistant with tool_calls"),
    }
    match &result[3] {
        ConversationItem::ToolResult(t) => assert_eq!(t.content.as_ref(), "fn main() {}"),
        _ => panic!("expected ToolResult to survive"),
    }
}
/// Reasoning kept on non-Messages backends, stripped on Messages — tool I/O survives either way.
#[test]
fn verbatim_reasoning_kept_unless_messages_backend() {
    use echo_agent_sampling_types::{ToolCall, rs};
    let mk = || {
        vec![
            ConversationItem::system("sys"),
            ConversationItem::Reasoning(rs::ReasoningItem {
                id: "r1".to_string(),
                summary: vec![],
                content: None,
                encrypted_content: Some("sig".to_string()),
                status: None,
            }),
            ConversationItem::assistant_tool_calls(vec![ToolCall {
                id: "c1".into(),
                name: "grep".to_string(),
                arguments: "{}".into(),
            }]),
            ConversationItem::tool_result("c1", "match"),
        ]
    };
    let kept = prepare_conversation_for_verbatim_summarization(mk(), false);
    assert!(
        kept.iter()
            .any(|i| matches!(i, ConversationItem::Reasoning(_))),
        "reasoning must be kept when strip_reasoning = false (EchoAgent backends)"
    );
    let stripped = prepare_conversation_for_verbatim_summarization(mk(), true);
    assert!(
        !stripped
            .iter()
            .any(|i| matches!(i, ConversationItem::Reasoning(_))),
        "reasoning must be stripped when strip_reasoning = true (Messages backend)"
    );
    assert!(
        stripped
            .iter()
            .any(|i| matches!(i, ConversationItem::ToolResult(_))),
        "tool results must survive even when reasoning is stripped"
    );
}
/// A trailing incomplete `tool_calls` turn is dropped; an earlier complete run is preserved.
#[test]
fn verbatim_truncates_trailing_incomplete_tool_call() {
    use echo_agent_sampling_types::ToolCall;
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("go"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "c1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"target_file":"a.rs"}"#.into(),
        }]),
        ConversationItem::tool_result("c1", "fn main() {}"),
        // Trailing, no matching ToolResult — results never arrived.
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "c2".into(),
            name: "grep".to_string(),
            arguments: "{}".into(),
        }]),
    ];
    let result = prepare_conversation_for_verbatim_summarization(conv, false);
    assert_eq!(
        result.len(),
        4,
        "trailing incomplete tool call must be dropped"
    );
    assert!(matches!(
        result.last(),
        Some(ConversationItem::ToolResult(_))
    ));
}
/// A conversation ending in a complete tool run (tail = `ToolResult`) is left untouched.
#[test]
fn verbatim_keeps_trailing_complete_tool_run() {
    use echo_agent_sampling_types::ToolCall;
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "c1".into(),
            name: "read_file".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("c1", "ok"),
    ];
    let result = prepare_conversation_for_verbatim_summarization(conv, false);
    assert_eq!(result.len(), 3, "complete trailing run must be preserved");
}
/// A conversation already within budget is returned unchanged.
#[test]
fn fit_returns_unchanged_when_within_budget() {
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hi"),
        ConversationItem::assistant("hello"),
    ];
    let out = fit_conversation_to_budget(conv, 1_000_000);
    assert_eq!(out.len(), 3);
}
/// Over budget: oldest whole turns dropped; System and most-recent turns survive.
#[test]
fn fit_drops_oldest_turns_keeps_system_and_recent() {
    let big = "x".repeat(800);
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::user(&big),      // old + large -> dropped
        ConversationItem::assistant(&big), // old + large -> dropped
        ConversationItem::user("recent question"),
        ConversationItem::assistant("recent answer"),
    ];
    let out = fit_conversation_to_budget(conv, 60);
    assert!(
        matches!(out.first(), Some(ConversationItem::System(_))),
        "system must be kept"
    );
    assert!(
        out.iter().any(|i| i.text_content() == "recent answer"),
        "most-recent turn must be kept"
    );
    assert!(
        !out.iter().any(|i| i.text_content().len() > 100),
        "the large old turns must be dropped"
    );
}
/// Trimming must not leave a leading orphan `ToolResult` whose assistant turn was dropped.
#[test]
fn fit_drops_leading_orphan_tool_result() {
    use echo_agent_sampling_types::ToolCall;
    let big = "y".repeat(2000);
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "c1".into(),
            name: "read_file".to_string(),
            arguments: big.into(),
        }]),
        ConversationItem::tool_result("c1", "result-old"),
        ConversationItem::user("recent"),
    ];
    let out = fit_conversation_to_budget(conv, 5);
    assert!(
        !out.iter()
            .any(|i| matches!(i, ConversationItem::ToolResult(_))),
        "orphaned tool result (its assistant turn was trimmed) must be dropped"
    );
    assert!(matches!(out.first(), Some(ConversationItem::System(_))));
}
/// An oversized most-recent tool result is kept but truncated in place (with its `tool_use`), not dropped.
#[test]
fn fit_truncates_oversized_tail_result_in_place() {
    use echo_agent_sampling_types::ToolCall;
    let huge = "z".repeat(40_000);
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("old"),
        ConversationItem::assistant("old answer"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "c1".into(),
            name: "read_file".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("c1", huge.as_str()), // triggering result
    ];
    let out = fit_conversation_to_budget(conv, 100);
    let tr = out
        .iter()
        .find_map(|i| match i {
            ConversationItem::ToolResult(t) => Some(t),
            _ => None,
        })
        .expect("triggering tool result must be kept (truncated), not dropped");
    assert!(
        tr.content.contains("truncated"),
        "kept result must carry a truncation marker"
    );
    assert!(
        tr.content.len() < huge.len(),
        "kept result content must be shortened"
    );
    assert!(
        out.iter()
            .any(|i| matches!(i, ConversationItem::Assistant(a) if !a.tool_calls.is_empty())),
        "owning assistant tool_use must be kept so the result is not orphaned"
    );
    let est: u64 = out.iter().map(estimate_item_tokens).sum();
    assert!(
        est <= 100 + 64,
        "truncated unit should fit budget (+ marker slack)"
    );
}
/// A single oversized trailing text turn is also truncated in place, not dropped.
#[test]
fn fit_truncates_oversized_tail_text_item() {
    let huge = "q".repeat(40_000);
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("old"),
        ConversationItem::assistant(huge.as_str()),
    ];
    let out = fit_conversation_to_budget(conv, 100);
    match out.last().expect("tail kept") {
        ConversationItem::Assistant(a) => {
            assert!(a.content.contains("truncated"));
            assert!(a.content.len() < huge.len());
        }
        other => panic!("expected truncated trailing assistant, got {other:?}"),
    }
}
/// Incompactable-state regression: `fit` must charge images (765 each), so an image-heavy old turn is trimmed.
#[test]
fn fit_counts_user_images_against_budget() {
    use echo_agent_sampling_types::ContentPart;
    let mut img_user = ConversationItem::user("");
    for _ in 0..50 {
        img_user.add_image("data:image/png;base64,AAAA");
    }
    let conv = vec![
        ConversationItem::system("sys"),
        img_user, // old turn, huge by image charges, ~0 by text bytes
        ConversationItem::user("recent question"),
        ConversationItem::assistant("recent answer"),
    ];
    let out = fit_conversation_to_budget(conv, 1_000);
    assert!(
        !out.iter().any(|i| matches!(
            i,
            ConversationItem::User(u)
                if u.content.iter().any(|p| matches!(p, ContentPart::Image { .. }))
        )),
        "image-heavy old turn must be counted (765/image) and trimmed, not kept"
    );
    assert!(
        out.iter().any(|i| i.text_content() == "recent answer"),
        "recent turn must survive"
    );
}
/// Incompactable-state regression: `fit` must charge encrypted-reasoning bytes (enc/4), so the old turn is trimmed.
#[test]
fn fit_counts_encrypted_reasoning_against_budget() {
    use echo_agent_sampling_types::rs;
    let big_enc = "Z".repeat(40_000);
    let reasoning = ConversationItem::Reasoning(rs::ReasoningItem {
        id: "r1".to_string(),
        summary: vec![],
        content: None,
        encrypted_content: Some(big_enc),
        status: None,
    });
    let conv = vec![
        ConversationItem::system("sys"),
        reasoning, // old turn, huge by encrypted bytes, 0 by visible text
        ConversationItem::user("recent question"),
        ConversationItem::assistant("recent answer"),
    ];
    let out = fit_conversation_to_budget(conv, 1_000);
    assert!(
        !out.iter()
            .any(|i| matches!(i, ConversationItem::Reasoning(_))),
        "encrypted-reasoning bytes must be counted and the old turn trimmed"
    );
    assert!(
        out.iter().any(|i| i.text_content() == "recent answer"),
        "recent turn must survive"
    );
}

#[test]
fn retrieval_query_stays_separate_from_knowledge_injection() {
    let mut user = ConversationItem::user(format!(
        "<user_query><echoagent_personal_knowledge>{}</echoagent_personal_knowledge>原问题</user_query>",
        "资料".repeat(3000)
    ));
    if let ConversationItem::User(item) = &mut user {
        item.retrieval_query = Some("原问题".into());
    }
    assert_eq!(extract_last_real_user_query(&[user]), Some("原问题".into()));
    assert_eq!(
        extract_user_query(
            "<user_query><echoagent_personal_knowledge>旧资料</echoagent_personal_knowledge>原问题</user_query>"
        ),
        "原问题"
    );
}
#[test]
fn organization_answers_cannot_become_personal_memory() {
    let mut restricted = ConversationItem::user("组织制度是什么？");
    if let ConversationItem::User(item) = &mut restricted {
        item.knowledge_context = Some("organization-boundary".into());
    }
    let safe = personal_memory_conversation(vec![
        restricted,
        ConversationItem::assistant("受限组织内容"),
        ConversationItem::user("以后回答简洁一点"),
        ConversationItem::assistant("好的"),
    ]);
    assert_eq!(safe.len(), 2);
    assert!(
        safe.iter().all(is_real_user_turn),
        "retain human preferences while excluding all knowledge-derived answers"
    );
    assert!(
        !safe
            .iter()
            .any(|item| item.text_content().contains("受限组织内容"))
    );
    assert_eq!(
        extract_last_real_user_query(&safe),
        Some("以后回答简洁一点".into())
    );
}

#[test]
fn retrieved_provenance_survives_compaction_and_disk_roundtrip() {
    let state = CompactionStateContext {
        knowledge_context: Some("organization-account-scope".into()),
        cwd_generation: 0,
        destination_project_instructions: None,
        agent_message_anchor: None,
        recent_messages: vec![],
        last_user_query: Some("组织制度是什么？".into()),
        agent_edited_paths: vec![],
        running_tasks: vec![],
        running_subagents: vec![],
        connected_mcp_servers: vec![],
        todos: vec![],
    };
    let compacted = build_compacted_history(CompactedHistoryInput {
        system_message: ConversationItem::system("sys"),
        user_message_prefix: String::new(),
        agents_md_reminder: None,
        state_context: &state.for_compaction(),
        compaction_summary: "组织制度的受限结论".into(),
        system_reminder: None,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    });
    let bytes = serde_json::to_vec(&compacted).unwrap();
    let mut restored: Vec<ConversationItem> = serde_json::from_slice(&bytes).unwrap();
    restored.push(ConversationItem::user("继续解释刚才的制度"));
    restored.push(ConversationItem::assistant("基于组织资料的另一个结论"));
    let safe = personal_memory_conversation(restored);
    assert_eq!(
        extract_real_user_queries(&safe),
        vec!["组织制度是什么？", "继续解释刚才的制度"]
    );
    assert!(safe.iter().all(is_real_user_turn));
    assert!(!safe.iter().any(|item| item.text_content().contains("结论")));
}
