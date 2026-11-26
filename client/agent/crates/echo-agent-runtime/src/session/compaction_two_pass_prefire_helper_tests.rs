use super::{fingerprint_prefix, prefire_lead_percent};
use echo_agent_sampling_types::ConversationItem;

#[test]
fn fingerprint_stable_for_same_prefix() {
    let items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hello"),
        ConversationItem::assistant("hi"),
    ];
    assert_eq!(fingerprint_prefix(&items), fingerprint_prefix(&items));
}

#[test]
fn fingerprint_changes_when_prefix_content_changes() {
    let base = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hello"),
    ];
    let edited = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("HELLO there"), // a real edit/rewind of the prefix
    ];
    assert_ne!(
        fingerprint_prefix(&base),
        fingerprint_prefix(&edited),
        "a changed prefix must invalidate the cached NOTE1 fingerprint"
    );
}

#[test]
fn fingerprint_changes_with_length() {
    let short = vec![ConversationItem::user("a")];
    let long = vec![
        ConversationItem::user("a"),
        ConversationItem::assistant("b"),
    ];
    assert_ne!(fingerprint_prefix(&short), fingerprint_prefix(&long));
}

#[test]
fn prefire_lead_percent_defaults_to_10() {
    // SAFETY: single-threaded test mutation of our own env var.
    unsafe { std::env::remove_var("ECHO_AGENT_PREFIRE_LEAD_PERCENT") };
    assert_eq!(prefire_lead_percent(), 10);
}
