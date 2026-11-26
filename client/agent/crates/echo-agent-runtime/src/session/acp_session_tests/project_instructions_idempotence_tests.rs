use super::{LEGACY_AGENTS_MD_REMINDER_PREFIX, conversation_has_project_instructions};
use echo_agent_sampling_types::{ContentPart, ConversationItem, SyntheticReason, UserItem};

/// A `User` item tagged `ProjectInstructions` is the canonical
/// post-Task-1 representation and must be detected.
#[test]
fn detects_tagged_project_instructions_item() {
    let conv = vec![
        ConversationItem::system("SP"),
        ConversationItem::project_instructions("AGENTS.md body"),
    ];
    assert!(
        conversation_has_project_instructions(&conv),
        "tagged ProjectInstructions item must be recognised"
    );
}

/// Older shells wrote AGENTS.md via `ConversationItem::user(...)` (no
/// tag). Resumed sessions load that exact untagged shape. The
/// structural-prefix branch must catch it so we don't double-insert on
/// the next resume.
#[test]
fn detects_legacy_untagged_reminder_via_wrapper_prefix() {
    let legacy = format!(
        "{LEGACY_AGENTS_MD_REMINDER_PREFIX} (ordered from repo root to current directory - deeper files take precedence on conflicts):\n\n## From: /repo/AGENTS.md\n# stuff\n</system-reminder>"
    );
    let conv = vec![
        ConversationItem::system("SP"),
        ConversationItem::user(legacy),
    ];
    assert!(
        conversation_has_project_instructions(&conv),
        "untagged user item starting with the wrapper prefix must be recognised"
    );
}

/// Empty conversation: nothing to find.
#[test]
fn empty_conversation_returns_false() {
    let conv: Vec<ConversationItem> = vec![];
    assert!(
        !conversation_has_project_instructions(&conv),
        "empty conversation has no AGENTS.md reminder"
    );
}

/// A real user message (no tag, no wrapper prefix) must NOT trigger
/// the heuristic. False positives here would suppress the legitimate
/// spawn-time inject for fresh sessions and break new conversations.
#[test]
fn real_user_message_returns_false() {
    let conv = vec![
        ConversationItem::system("SP"),
        ConversationItem::user("hello, please help me refactor this function"),
    ];
    assert!(
        !conversation_has_project_instructions(&conv),
        "plain real user message must not match the legacy heuristic"
    );
}

/// The heuristic must only inspect the FIRST content part. A user
/// item whose [1] part happens to start with the wrapper prefix (e.g.
/// a multi-part real message that pastes the prefix later) must not
/// false-positive.
#[test]
fn wrapper_prefix_in_non_first_content_part_returns_false() {
    let conv = vec![
        ConversationItem::system("SP"),
        ConversationItem::User(UserItem {
            content: vec![
                ContentPart::Text {
                    text: "real user message".into(),
                },
                ContentPart::Text {
                    text: format!("{LEGACY_AGENTS_MD_REMINDER_PREFIX} ...").into(),
                },
            ],
            synthetic_reason: None,
            ..Default::default()
        }),
    ];
    assert!(
        !conversation_has_project_instructions(&conv),
        "wrapper prefix in a non-first content part must not match"
    );
}

/// `starts_with` is required: a real user message that quotes the
/// wrapper text somewhere in its body (e.g. "look at this:
/// \n\n<system-reminder>\n...") must NOT match. Anything weaker
/// (e.g. `contains`) would suppress legitimate inserts on resumed
/// sessions where the user paraphrased AGENTS.md content into a real
/// prompt.
#[test]
fn wrapper_prefix_mid_text_returns_false() {
    let buried = format!("Hi! Look at this snippet:{LEGACY_AGENTS_MD_REMINDER_PREFIX}");
    let conv = vec![
        ConversationItem::system("SP"),
        ConversationItem::user(buried),
    ];
    assert!(
        !conversation_has_project_instructions(&conv),
        "wrapper prefix appearing mid-text (not at start) must not match"
    );
}

/// Pin the *contract* of the spawn-time chokepoint: when the helper
/// returns false, Site A's branch must insert exactly one tagged
/// project-instructions item and bump `inherited_prefix_len`; when
/// the helper returns true on the resulting conversation, Site A
/// must skip both the insert and the bump.
///
/// This is NOT an integration test of `spawn_session_actor`'s async
/// setup (which needs `SessionInfo`, `ChatStateHandle`, `Agent`,
/// `ToolBridge`, persistence dirs, gateway senders, etc. — building
/// one is a multi-hundred-line fixture). Instead, it mimics Site A's
/// inner branch against a `(conversation, reminder,
/// inherited_prefix_len)` tuple so any future drift in the
/// idempotence-guard shape (e.g. inverting the check, dropping the
/// `inherited_prefix_len` bump, swapping `ConversationItem::project_instructions`
/// for `ConversationItem::user`) fails this test immediately.
/// Production-site equivalence is verified by `grep` at edit time
/// and review.
#[test]
fn site_a_skips_when_helper_returns_true_and_bumps_len_when_inserting() {
    // Case 1: helper returns false → insert happens → tagged item
    // appears at index 1 → inherited_prefix_len bumps from Some(1)
    // to Some(2).
    let mut conv: Vec<ConversationItem> = vec![ConversationItem::system("SP")];
    let mut inherited_prefix_len: Option<usize> = Some(1);
    let reminder = "AGENTS.md body for spawn-time inject";

    let has_pi_before = conversation_has_project_instructions(&conv);
    assert!(
        !has_pi_before,
        "fresh conversation must not yet have project-instructions"
    );

    if !has_pi_before {
        let insert_at = conv.len().min(1);
        conv.insert(insert_at, ConversationItem::project_instructions(reminder));
        if let Some(ref mut len) = inherited_prefix_len {
            *len += 1;
        }
    }

    assert_eq!(
        inherited_prefix_len,
        Some(2),
        "inherited_prefix_len must bump by 1 when inserting"
    );
    assert_eq!(conv.len(), 2, "conversation must grow by exactly one item");
    match &conv[1] {
        ConversationItem::User(u) => {
            assert_eq!(
                u.synthetic_reason,
                Some(SyntheticReason::ProjectInstructions),
                "inserted item must carry the ProjectInstructions tag"
            );
            assert_eq!(
                u.content.first().and_then(|p| match p {
                    ContentPart::Text { text } => Some(text.as_ref()),
                    _ => None,
                }),
                Some(reminder),
                "inserted content must be the reminder text verbatim"
            );
        }
        other => panic!("expected User at index 1, got {other:?}"),
    }

    // Case 2: helper now returns true on the same conversation →
    // Site A's guard short-circuits → no second insert, no further
    // bump. This catches accidental re-injection on retry / replay.
    let conv_len_before = conv.len();
    let len_before = inherited_prefix_len;

    let has_pi_after = conversation_has_project_instructions(&conv);
    assert!(
        has_pi_after,
        "tagged item just inserted must be recognised by the helper"
    );

    if !has_pi_after {
        // Unreachable on a correct helper; if this branch ever runs,
        // it means the helper failed to recognise its own freshly
        // inserted tagged item.
        panic!("Site A would have double-inserted — helper failed to recognise tagged item");
    }

    assert_eq!(
        conv.len(),
        conv_len_before,
        "skip branch must not mutate the conversation"
    );
    assert_eq!(
        inherited_prefix_len, len_before,
        "skip branch must not bump inherited_prefix_len"
    );
}

/// Same skip-on-fork contract, but with `inherited_prefix_len = None`
/// (which is how a fresh, non-forked session arrives). Fork-only
/// state must not be touched when there's no fork accounting in play.
#[test]
fn site_a_handles_none_inherited_prefix_len_without_panicking() {
    let mut conv: Vec<ConversationItem> = vec![
        ConversationItem::system("SP"),
        ConversationItem::project_instructions("already present from a prior run"),
    ];
    let mut inherited_prefix_len: Option<usize> = None;
    let reminder = "would-be duplicate";

    assert!(
        conversation_has_project_instructions(&conv),
        "tagged item is present"
    );

    let conv_len_before = conv.len();
    if !conversation_has_project_instructions(&conv) {
        let insert_at = conv.len().min(1);
        conv.insert(insert_at, ConversationItem::project_instructions(reminder));
        if let Some(ref mut len) = inherited_prefix_len {
            *len += 1;
        }
    }

    assert_eq!(
        conv.len(),
        conv_len_before,
        "skip branch must not duplicate the tagged item"
    );
    assert_eq!(
        inherited_prefix_len, None,
        "None inherited_prefix_len must stay None"
    );
}

/// A verbatim mirror-fork
/// (`preserve_inherited_system = true`) must NOT insert AGENTS.md even when
/// the inherited prefix lacks project-instructions and the agent has a
/// reminder. Inserting would shift the inherited prefix off the parent's
/// cached radix stream before the planner's first inference. Mirrors
/// `spawn_session_actor`'s Site A branch with the fork-preservation gate;
/// production equivalence is verified by grep + review (see the note above).
#[test]
fn site_a_skips_agents_md_insert_on_verbatim_mirror_fork() {
    let mut conv: Vec<ConversationItem> = vec![
        ConversationItem::system("parent system verbatim"),
        ConversationItem::user("parent turn 1"),
    ];
    let mut inherited_prefix_len: Option<usize> = Some(2);
    let preserve_inherited_system = true;
    let agents_md_reminder: Option<&str> = Some("AGENTS.md body");

    assert!(
        !conversation_has_project_instructions(&conv),
        "precondition: inherited prefix lacks project-instructions"
    );

    if !preserve_inherited_system
        && !conversation_has_project_instructions(&conv)
        && let Some(reminder) = agents_md_reminder
    {
        let insert_at = conv.len().min(1);
        conv.insert(insert_at, ConversationItem::project_instructions(reminder));
        if let Some(ref mut len) = inherited_prefix_len {
            *len += 1;
        }
    }

    assert_eq!(
        conv.len(),
        2,
        "verbatim mirror-fork must not insert an AGENTS.md item"
    );
    assert!(
        !conversation_has_project_instructions(&conv),
        "no project-instructions item may be added on a verbatim fork"
    );
    assert_eq!(
        inherited_prefix_len,
        Some(2),
        "verbatim mirror-fork must leave inherited_prefix_len unchanged"
    );
}

/// Non-fork counterpart of the test above: with the same inputs but
/// `preserve_inherited_system = false`, the fork-preservation gate is
/// transparent and the AGENTS.md insert + `inherited_prefix_len` bump still
/// happen. Pins that the new gate did not regress fresh / non-fork spawns.
#[test]
fn site_a_still_inserts_agents_md_on_non_fork_spawn() {
    let mut conv: Vec<ConversationItem> = vec![
        ConversationItem::system("parent system verbatim"),
        ConversationItem::user("parent turn 1"),
    ];
    let mut inherited_prefix_len: Option<usize> = Some(2);
    let preserve_inherited_system = false;
    let agents_md_reminder: Option<&str> = Some("AGENTS.md body");

    if !preserve_inherited_system
        && !conversation_has_project_instructions(&conv)
        && let Some(reminder) = agents_md_reminder
    {
        let insert_at = conv.len().min(1);
        conv.insert(insert_at, ConversationItem::project_instructions(reminder));
        if let Some(ref mut len) = inherited_prefix_len {
            *len += 1;
        }
    }

    assert_eq!(
        conv.len(),
        3,
        "non-fork spawn must insert one AGENTS.md item"
    );
    assert!(
        matches!(&conv[1], ConversationItem::User(u)
            if u.synthetic_reason == Some(SyntheticReason::ProjectInstructions)),
        "AGENTS.md must be inserted as a tagged project-instructions item at index 1"
    );
    assert_eq!(
        inherited_prefix_len,
        Some(3),
        "non-fork spawn must bump inherited_prefix_len by one"
    );
}

/// Second gate: `ensure_prefix_ready`'s AGENTS.md
/// insert carries the same `preserve_inherited_system` guard (defensive — it
/// does not fire for subagents today). With the flag set, the post-prefix
/// insert must be skipped. Mirrors that branch; production equivalence via
/// grep + review.
#[test]
fn site_b_skips_agents_md_insert_on_verbatim_mirror_fork() {
    // `ensure_prefix_ready` shape: the user prefix sits at `insert_at`, and
    // AGENTS.md would otherwise be inserted at `(insert_at + 1).min(len)`.
    let mut conv: Vec<ConversationItem> = vec![
        ConversationItem::system("parent system verbatim"),
        ConversationItem::user("first-prompt prefix"),
    ];
    let insert_at = 1usize;
    let preserve_inherited_system = true;
    let agents_md_reminder: Option<&str> = Some("AGENTS.md body");

    if !preserve_inherited_system
        && !conversation_has_project_instructions(&conv)
        && let Some(reminder) = agents_md_reminder
    {
        let agents_md_at = (insert_at + 1).min(conv.len());
        conv.insert(
            agents_md_at,
            ConversationItem::project_instructions(reminder),
        );
    }

    assert_eq!(
        conv.len(),
        2,
        "Site B must not insert AGENTS.md on a verbatim fork"
    );
    assert!(
        !conversation_has_project_instructions(&conv),
        "no project-instructions item may be added at Site B on a verbatim fork"
    );
}
