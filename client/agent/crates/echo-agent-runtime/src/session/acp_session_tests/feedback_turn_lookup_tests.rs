use super::turn_texts_for_feedback;
use echo_agent_sampling_types::ConversationItem;

#[test]
fn empty_conversation_returns_none() {
    let conv: Vec<ConversationItem> = vec![];
    assert_eq!(turn_texts_for_feedback(&conv, 0), (None, None));
}

#[test]
fn turn_zero_returns_first_exchange() {
    let conv = vec![
        ConversationItem::user("q1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("q2"),
        ConversationItem::assistant("a2"),
    ];
    assert_eq!(
        turn_texts_for_feedback(&conv, 0),
        (Some("q1".into()), Some("a1".into()))
    );
}

#[test]
fn turn_n_returns_nth_exchange() {
    let conv = vec![
        ConversationItem::user("q1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("q2"),
        ConversationItem::assistant("a2"),
        ConversationItem::user("q3"),
        ConversationItem::assistant("a3"),
    ];
    assert_eq!(
        turn_texts_for_feedback(&conv, 1),
        (Some("q2".into()), Some("a2".into()))
    );
}

#[test]
fn out_of_range_returns_none() {
    let conv = vec![
        ConversationItem::user("only q"),
        ConversationItem::assistant("only a"),
    ];
    assert_eq!(turn_texts_for_feedback(&conv, 5), (None, None));
}

#[test]
fn no_assistant_yet_returns_user_only() {
    // q2 has no assistant response yet.
    let conv = vec![
        ConversationItem::user("q1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("q2"),
    ];
    assert_eq!(turn_texts_for_feedback(&conv, 1), (Some("q2".into()), None));
}

#[test]
fn does_not_bleed_assistant_into_next_turn() {
    // Turn 0 (q1) has no assistant; turn 1 (q2) does. The lookup for
    // turn 0 must NOT pick up turn 1's assistant.
    let conv = vec![
        ConversationItem::user("q1"),
        ConversationItem::user("q2"),
        ConversationItem::assistant("a2"),
    ];
    assert_eq!(turn_texts_for_feedback(&conv, 0), (Some("q1".into()), None));
}

#[test]
fn skips_whitespace_only_assistant() {
    let conv = vec![
        ConversationItem::user("q"),
        ConversationItem::assistant("   \n  "),
        ConversationItem::assistant("real answer"),
    ];
    assert_eq!(
        turn_texts_for_feedback(&conv, 0),
        (Some("q".into()), Some("real answer".into()))
    );
}

/// CRITICAL: per-turn feedback must render the same `*User:*` text in
/// Slack as latest-turn feedback (which goes through `extract_user_query`).
/// Without this stripping the same channel sees raw `<user_query>` blobs
/// from per-turn submissions and clean prose from spontaneous ones.
#[test]
fn strips_user_query_metadata_tags() {
    let raw = "<user_info>internal</user_info><user_query>fix the bug</user_query><project_layout>tree</project_layout>";
    let conv = vec![
        ConversationItem::user(raw),
        ConversationItem::assistant("on it"),
    ];
    assert_eq!(
        turn_texts_for_feedback(&conv, 0),
        (Some("fix the bug".into()), Some("on it".into()))
    );
}
