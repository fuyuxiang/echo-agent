use super::*;
use echo_agent_sampling_types::{ContentPart, SyntheticReason};

fn armed() -> FailedResponseCapture {
    FailedResponseCapture::armed()
}

/// A capture that has seen a terminal `output` list, the way the Responses
/// stream records one.
fn armed_with_terminal(output: Vec<rs::OutputItem>) -> FailedResponseCapture {
    let capture = armed();
    capture.record_terminal_output(&output);
    capture
}

fn message_item(id: &str, text: &str) -> rs::OutputItem {
    rs::OutputItem::Message(rs::OutputMessage {
        content: vec![rs::OutputMessageContent::OutputText(
            rs::OutputTextContent {
                annotations: Vec::new(),
                text: text.into(),
                logprobs: None,
            },
        )],
        id: id.into(),
        role: rs::AssistantRole::Assistant,
        status: rs::OutputStatus::Completed,
    })
}

fn function_call_item(call_id: &str) -> rs::OutputItem {
    rs::OutputItem::FunctionCall(rs::FunctionToolCall {
        arguments: "{}".into(),
        call_id: call_id.into(),
        id: Some(call_id.into()),
        name: "read_file".into(),
        status: Some(rs::OutputStatus::Completed),
    })
}

fn reasoning_item(id: &str, content: Option<&str>, summary: Option<&str>) -> rs::ReasoningItem {
    rs::ReasoningItem {
        id: id.into(),
        summary: summary
            .map(|text| {
                vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text: text.into(),
                })]
            })
            .unwrap_or_default(),
        content: content.map(|text| vec![rs::ReasoningTextContent { text: text.into() }]),
        encrypted_content: None,
        status: None,
    }
}

#[test]
fn done_values_replace_deltas_without_duplication() {
    let capture = armed();
    capture.record_reasoning_summary_delta(0, 0, "reasoning-1".into(), "summary ");
    capture.record_reasoning_summary_done(0, 0, "reasoning-1".into(), "full summary".into());
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), "partial ");
    capture.record_reasoning_done(0, 0, "reasoning-1".into(), "full reasoning".into());
    capture.record_output_delta(1, 0, "message-1".into(), "partial ");
    capture.record_output_done(1, 0, "message-1".into(), "full answer".into());

    let items = capture.take_items();
    assert_eq!(items.len(), 2);
    let ConversationItem::Reasoning(reasoning) = &items[0] else {
        panic!("expected reasoning item");
    };
    assert_eq!(reasoning.id, "reasoning-1");
    assert_eq!(
        reasoning.content.as_ref().unwrap()[0].text,
        "full reasoning"
    );
    let ConversationItem::Assistant(assistant) = &items[1] else {
        panic!("expected assistant item");
    };
    assert_eq!(assistant.content.as_ref(), "full answer");
}

#[test]
fn summary_is_used_when_raw_reasoning_is_absent() {
    let capture = armed();
    capture.record_reasoning_summary_delta(0, 0, "reasoning-1".into(), "summary ");
    capture.record_reasoning_summary_done(0, 0, "reasoning-1".into(), "full summary".into());

    let items = capture.take_items();
    let ConversationItem::Reasoning(reasoning) = &items[0] else {
        panic!("expected reasoning item");
    };
    assert_eq!(reasoning.content.as_ref().unwrap()[0].text, "full summary");
}

/// A raw reasoning event that carries no text must not displace the summary:
/// the retry would otherwise lose the thought it exists to replay.
#[test]
fn an_empty_raw_reasoning_event_keeps_the_summary() {
    let capture = armed();
    capture.record_reasoning_summary_done(0, 0, "reasoning-1".into(), "the summary".into());
    capture.record_reasoning_done(0, 0, "reasoning-1".into(), String::new());
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), "");

    let items = capture.take_items();
    let ConversationItem::Reasoning(reasoning) = &items[0] else {
        panic!("expected the summary to survive");
    };
    assert_eq!(reasoning.content.as_ref().unwrap()[0].text, "the summary");
}

/// Same when the summary already spent the reasoning budget: the raw event
/// records nothing, so the summary stays the recovery context.
#[test]
fn a_budget_spent_raw_event_keeps_the_summary() {
    let capture = armed();
    capture.record_reasoning_summary_delta(
        0,
        0,
        "reasoning-1".into(),
        &"summary ".repeat(MAX_RECOVERY_REASONING_BYTES),
    );
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), "raw text past the cap");

    let items = capture.take_items();
    let ConversationItem::Reasoning(reasoning) = &items[0] else {
        panic!("expected the summary to survive");
    };
    let text = &reasoning.content.as_ref().unwrap()[0].text;
    assert!(
        text.starts_with("summary "),
        "the summary is replayed: {text:.40}"
    );
    assert!(text.ends_with(TRUNCATION_MARKER));
}

/// The exact-cap edge: a summary that lands precisely on the budget leaves no
/// room for a later raw delta, which must record nothing at all — not a bare
/// truncation marker that would read as retained raw text and displace the
/// summary.
#[test]
fn a_raw_delta_with_no_room_left_records_nothing() {
    let capture = armed();
    let summary = "s".repeat(MAX_RECOVERY_REASONING_BYTES);
    capture.record_reasoning_summary_delta(0, 0, "reasoning-1".into(), &summary);
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), "raw text with nowhere to go");

    let items = capture.take_items();
    assert_eq!(items.len(), 1);
    let ConversationItem::Reasoning(reasoning) = &items[0] else {
        panic!("expected the summary to survive");
    };
    assert_eq!(reasoning.content.as_ref().unwrap()[0].text, summary);
}

/// A disarmed capture (every stream that is not an armed recovery attempt)
/// records nothing at all.
#[test]
fn disarmed_capture_records_nothing() {
    let capture = FailedResponseCapture::default();
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), "raw reasoning");
    capture.record_output_done(1, 0, "message-1".into(), "answer".into());
    capture.record_unreplayable();

    assert!(capture.take_items().is_empty());
}

#[test]
fn terminal_recovery_keeps_a_tool_free_turn() {
    let capture = armed_with_terminal(vec![
        rs::OutputItem::Reasoning(reasoning_item(
            "reasoning-1",
            Some("failed reasoning"),
            None,
        )),
        message_item("message-1", "failed output"),
    ]);

    let items = capture.take_items();
    assert_eq!(items.len(), 2);
    let ConversationItem::Assistant(assistant) = &items[1] else {
        panic!("expected assistant item");
    };
    assert_eq!(assistant.content.as_ref(), "failed output");
}

/// A turn that called a tool is dropped whole: replaying its reasoning
/// without the call would send an orphaned reasoning item.
#[test]
fn terminal_recovery_drops_a_turn_that_called_a_tool() {
    let capture = armed_with_terminal(vec![
        rs::OutputItem::Reasoning(reasoning_item(
            "reasoning-1",
            Some("failed reasoning"),
            None,
        )),
        message_item("message-1", "failed output"),
        function_call_item("call-1"),
    ]);

    assert!(capture.take_items().is_empty());
}

/// The veto reads the raw wire items, so it also catches the tool calls the
/// conversation form drops on the floor — an MCP call among them.
#[test]
fn terminal_recovery_drops_a_turn_that_called_an_mcp_tool() {
    let capture = armed_with_terminal(vec![
        rs::OutputItem::Reasoning(reasoning_item(
            "reasoning-1",
            Some("failed reasoning"),
            None,
        )),
        rs::OutputItem::McpCall(rs::MCPToolCall {
            arguments: "{}".into(),
            error: None,
            id: "mcp-1".into(),
            name: "search".into(),
            output: None,
            server_label: "docs".into(),
            approval_request_id: None,
            status: None,
        }),
    ]);

    assert!(
        capture.take_items().is_empty(),
        "an MCP turn cannot be replayed without its call"
    );
}

/// The same rule applies mid-stream: a captured turn that started a function
/// call replays nothing.
#[test]
fn streamed_recovery_drops_a_turn_that_started_a_tool_call() {
    let capture = armed();
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), "raw reasoning");
    capture.record_unreplayable();

    assert!(capture.take_items().is_empty());

    let capture = armed_with_terminal(vec![message_item("message-1", "failed output")]);
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), "raw reasoning");
    capture.record_unreplayable();
    assert!(capture.take_items().is_empty());
}

/// Streamed raw reasoning fills a final item that carried no content, without
/// discarding the item's `encrypted_content` or its unstreamed siblings.
#[test]
fn terminal_recovery_merges_streamed_reasoning_into_the_final_item() {
    let capture = armed();
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), "raw reasoning");
    let mut streamed_item = reasoning_item("reasoning-1", None, Some("summary only"));
    streamed_item.encrypted_content = Some("cipher-1".into());
    capture.record_terminal_output(&[
        rs::OutputItem::Reasoning(streamed_item),
        rs::OutputItem::Reasoning(reasoning_item("reasoning-2", Some("unstreamed"), None)),
        message_item("message-1", "failed output"),
    ]);

    let items = capture.take_items();
    assert_eq!(items.len(), 3);
    let ConversationItem::Reasoning(merged) = &items[0] else {
        panic!("expected reasoning item");
    };
    assert_eq!(merged.content.as_ref().unwrap()[0].text, "raw reasoning");
    assert_eq!(merged.encrypted_content.as_deref(), Some("cipher-1"));
    let rs::SummaryPart::SummaryText(summary) = &merged.summary[0];
    assert_eq!(summary.text, "summary only");
    let ConversationItem::Reasoning(sibling) = &items[1] else {
        panic!("expected the unstreamed sibling to survive");
    };
    assert_eq!(sibling.content.as_ref().unwrap()[0].text, "unstreamed");
}

/// The opaque encrypted blob is charged to the replay budget: one that does
/// not fit is dropped rather than smuggling an unbounded turn into the retry.
#[test]
fn an_oversized_encrypted_blob_is_dropped() {
    let mut small = reasoning_item("reasoning-1", Some("short thought"), None);
    small.encrypted_content = Some("cipher".repeat(8));
    let mut oversized = reasoning_item("reasoning-2", Some("another thought"), None);
    oversized.encrypted_content = Some("c".repeat(MAX_RECOVERY_REASONING_BYTES + 1));
    let capture = armed_with_terminal(vec![
        rs::OutputItem::Reasoning(small),
        rs::OutputItem::Reasoning(oversized),
    ]);

    let items = capture.take_items();
    let ConversationItem::Reasoning(kept) = &items[0] else {
        panic!("expected reasoning item");
    };
    assert!(kept.encrypted_content.is_some(), "a blob that fits is kept");
    let ConversationItem::Reasoning(trimmed) = &items[1] else {
        panic!("expected reasoning item");
    };
    assert!(
        trimmed.encrypted_content.is_none(),
        "the oversized blob is dropped"
    );
    assert_eq!(
        trimmed.content.as_ref().unwrap()[0].text,
        "another thought",
        "dropping the blob does not cost the readable thought"
    );
}

/// Streamed reasoning the terminal response omitted is dropped: the response
/// fixes the item order, and an omitted item has no position to occupy.
#[test]
fn streamed_reasoning_the_wire_never_completed_replays_last() {
    let capture = armed();
    capture.record_reasoning_delta(2, 0, "reasoning-late".into(), "cut off mid-thought");
    capture.record_output_item(
        0,
        &rs::OutputItem::Reasoning(reasoning_item("reasoning-1", Some("first"), None)),
    );
    capture.record_output_item(1, &message_item("message-1", "the answer"));

    let items = capture.take_items();
    assert_eq!(items.len(), 3, "completed items first, then the cut item");
    let ConversationItem::Reasoning(first) = &items[0] else {
        panic!("the wire order is preserved");
    };
    assert_eq!(first.id, "reasoning-1");
    let ConversationItem::Assistant(assistant) = &items[1] else {
        panic!("expected assistant item");
    };
    assert_eq!(assistant.content.as_ref(), "the answer");
    let ConversationItem::Reasoning(cut) = &items[2] else {
        panic!("expected the uncompleted item from the deltas");
    };
    assert_eq!(cut.id, "reasoning-late");
}

/// A compaction item is opaque Responses state the retry cannot carry, so a
/// turn containing one is dropped whole rather than replayed against input
/// that no longer has the checkpoint its later items depend on.
#[test]
fn terminal_recovery_drops_a_turn_carrying_compaction_state() {
    let capture = armed_with_terminal(vec![
        rs::OutputItem::Compaction(rs::CompactionBody {
            id: "compaction-1".into(),
            encrypted_content: "checkpoint".into(),
            created_by: None,
        }),
        rs::OutputItem::Reasoning(reasoning_item("reasoning-1", Some("after the cut"), None)),
        message_item("message-1", "the answer"),
    ]);

    assert!(
        capture.take_items().is_empty(),
        "post-compaction items cannot be replayed without the checkpoint"
    );
}

/// Terminal authority does not depend on the projection keeping anything: an
/// empty terminal `output` still means the wire said everything, so stale
/// deltas are not resurrected behind it.
#[test]
fn an_empty_terminal_response_replays_nothing() {
    let capture = armed();
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), "streamed thought");
    capture.record_output_delta(1, 0, "message-1".into(), "streamed answer");
    capture.record_terminal_output(&[]);

    assert!(
        capture.take_items().is_empty(),
        "the authoritative response listed no items"
    );
}

/// A turn that reached its terminal frame has told us everything it
/// produced, so streamed reasoning the wire left out is not replayed.
#[test]
fn a_terminal_turn_does_not_gain_items_from_the_deltas() {
    let capture = armed();
    capture.record_reasoning_delta(0, 0, "reasoning-streamed".into(), "delta only");
    capture.record_terminal_output(&[
        rs::OutputItem::Reasoning(reasoning_item("reasoning-final", Some("the thought"), None)),
        message_item("message-1", "the answer"),
    ]);

    let items = capture.take_items();
    assert_eq!(items.len(), 2, "no item the wire left out: {items:?}");
    let ConversationItem::Reasoning(reasoning) = &items[0] else {
        panic!("expected reasoning item");
    };
    assert_eq!(reasoning.id, "reasoning-final");
}

/// A final item that carried its own content keeps it: the capture only fills
/// gaps, it never overwrites the authoritative turn.
#[test]
fn terminal_content_wins_over_streamed_text() {
    let capture = armed();
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), "streamed prefix");
    capture.record_terminal_output(&[rs::OutputItem::Reasoning(reasoning_item(
        "reasoning-1",
        Some("final reasoning"),
        None,
    ))]);

    let items = capture.take_items();
    let ConversationItem::Reasoning(reasoning) = &items[0] else {
        panic!("expected reasoning item");
    };
    assert_eq!(
        reasoning.content.as_ref().unwrap()[0].text,
        "final reasoning"
    );
}

/// A runaway thought is capped and marked, so repeated recovery attempts
/// cannot inflate the retry prompt without bound — and the separate text
/// budget keeps the answer the turn did produce.
#[test]
fn a_runaway_thought_is_capped_without_eliding_the_answer() {
    let capture = armed();
    let flood = "loop ".repeat(MAX_RECOVERY_REASONING_BYTES);
    capture.record_reasoning_delta(0, 0, "reasoning-1".into(), &flood);
    capture.record_output_delta(1, 0, "message-1".into(), "an answer past the thinking");

    let items = capture.take_items();
    assert_eq!(items.len(), 2, "the answer survives the capped thought");
    let ConversationItem::Reasoning(reasoning) = &items[0] else {
        panic!("expected reasoning item");
    };
    let text = &reasoning.content.as_ref().unwrap()[0].text;
    assert!(text.ends_with(TRUNCATION_MARKER), "truncation is marked");
    assert!(text.len() <= MAX_RECOVERY_REASONING_BYTES + TRUNCATION_MARKER.len());
    let ConversationItem::Assistant(assistant) = &items[1] else {
        panic!("expected assistant item");
    };
    assert_eq!(assistant.content.as_ref(), "an answer past the thinking");
}

/// A runaway visible answer is capped on its own budget.
#[test]
fn a_runaway_answer_is_capped_and_marked() {
    let capture = armed();
    capture.record_output_delta(
        0,
        0,
        "message-1".into(),
        &"chatter ".repeat(MAX_RECOVERY_TEXT_BYTES),
    );

    let items = capture.take_items();
    let ConversationItem::Assistant(assistant) = &items[0] else {
        panic!("expected assistant item");
    };
    assert!(assistant.content.ends_with(TRUNCATION_MARKER));
    assert!(assistant.content.len() <= MAX_RECOVERY_TEXT_BYTES + TRUNCATION_MARKER.len());
}

/// The terminal path caps each channel the same way, across all of the
/// turn's items.
#[test]
fn a_runaway_terminal_turn_is_capped_per_channel() {
    let flood = "loop ".repeat(MAX_RECOVERY_REASONING_BYTES);
    let capture = armed_with_terminal(vec![
        rs::OutputItem::Reasoning(reasoning_item("reasoning-1", Some(&flood), None)),
        rs::OutputItem::Reasoning(reasoning_item("reasoning-2", Some(&flood), None)),
        message_item("message-1", &flood),
    ]);

    let items = capture.take_items();
    let reasoning_bytes: usize = items
        .iter()
        .filter_map(|item| match item {
            ConversationItem::Reasoning(reasoning) => Some(
                reasoning
                    .content
                    .as_ref()
                    .map_or(0, |parts| parts.iter().map(|part| part.text.len()).sum()),
            ),
            _ => None,
        })
        .sum();
    let text_bytes: usize = items
        .iter()
        .filter_map(|item| match item {
            ConversationItem::Assistant(assistant) => Some(assistant.content.len()),
            _ => None,
        })
        .sum();
    assert!(reasoning_bytes <= MAX_RECOVERY_REASONING_BYTES + TRUNCATION_MARKER.len());
    assert!(text_bytes > 0, "the answer is never elided by the thought");
    assert!(text_bytes <= MAX_RECOVERY_TEXT_BYTES + TRUNCATION_MARKER.len());
}

#[test]
fn append_recovery_context_uses_synthetic_user_reminder() {
    let mut request = ConversationRequest::default();
    append_recovery_context(
        &mut request,
        vec![ConversationItem::assistant("failed output")],
    );

    assert_eq!(request.items.len(), 2);
    let ConversationItem::User(reminder) = &request.items[1] else {
        panic!("expected user-role reminder");
    };
    assert_eq!(
        reminder.synthetic_reason,
        Some(SyntheticReason::SystemReminder)
    );
    let [ContentPart::Text { text }] = reminder.content.as_slice() else {
        panic!("expected one text part");
    };
    assert_eq!(text.as_ref(), RECOVERY_REMINDER);
}
