//! Regression tests: rewind must remove the rewound turn even when the
//! session contains synthetic-origin turns (auto-wake task/subagent
//! completions, notification drains, scheduler fires).
//!
//! Those turns increment `prompt_index` but push a *synthetic* `User` item;
//! truncation that counts only non-synthetic `User` items therefore leaves
//! the "rewound" turn in the model's context.

use super::support::create_test_actor;

use crate::sampling::ConversationItem;
use crate::session::{RewindMode, RewindRequest};

/// Build the canonical bugged-session shape:
///
/// ```text
/// [Sys, User(user_info), U0(real), A0, U1(auto-wake, synthetic), A1, U2(real), A2]
/// prompt_index = 3, prompt_texts = [P0, TASK_WAKE, P2]
/// ```
///
/// Turn 1 is a background-task auto-wake (`PromptOrigin::TaskCompleted`):
/// it consumed a prompt index but its user item is synthetic.
fn seed_conversation(mark_turn_starts: bool) -> Vec<ConversationItem> {
    let turn_user = |text: &str, idx: usize| {
        let mut item = ConversationItem::user(text);
        if mark_turn_starts {
            item.set_prompt_index(idx);
        }
        item
    };
    let auto_wake = |text: &str, idx: usize| {
        let mut item = ConversationItem::task_completed(text);
        if mark_turn_starts {
            item.set_prompt_index(idx);
        }
        item
    };
    vec![
        ConversationItem::system("SYS"),
        ConversationItem::user("<user_info>OS: test</user_info>"),
        turn_user("P0", 0),
        ConversationItem::assistant("A0"),
        auto_wake("Background task abc completed", 1),
        ConversationItem::assistant("A1"),
        turn_user("P2", 2),
        ConversationItem::assistant("A2"),
    ]
}

async fn run_rewind_over_synthetic_turn(mark_turn_starts: bool) {
    let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

    let mut snap = actor
        .chat_state_handle
        .snapshot()
        .await
        .expect("snapshot available");
    snap.conversation = seed_conversation(mark_turn_starts);
    snap.prompt_index = 3;
    snap.prompt_texts = vec![
        "P0".into(),
        "Background task abc completed".into(),
        "P2".into(),
    ];
    snap.last_compaction_prompt_index = None;
    actor.chat_state_handle.restore_snapshot(snap);

    // Rewind to prompt #2 — "restore state before P2 ran".
    let resp = actor
        .handle_rewind(RewindRequest {
            target_prompt_index: 2,
            force: true,
            file_guard: None,
            mode: RewindMode::ConversationOnly,
        })
        .await
        .expect("handle_rewind ok");
    assert!(resp.success, "rewind should succeed: {resp:?}");
    assert_eq!(resp.prompt_text.as_deref(), Some("P2"));

    let conv = actor.chat_state_handle.get_conversation().await;
    let texts: Vec<String> = conv.iter().map(|c| c.text_content()).collect();

    assert!(
        !texts.iter().any(|t| t == "P2" || t == "A2"),
        "rewound turn must not stay in the model's context \
         (mark_turn_starts={mark_turn_starts}): {texts:?}"
    );
    assert_eq!(
        texts,
        vec![
            "SYS",
            "<user_info>OS: test</user_info>",
            "P0",
            "A0",
            "Background task abc completed",
            "A1",
        ],
        "conversation must keep prompts 0..=1 only"
    );
    assert_eq!(actor.chat_state_handle.get_prompt_index().await, 2);
}

/// Marker-less items (sessions persisted before `UserItem.prompt_index`
/// existed): the counting fallback must classify the synthetic auto-wake
/// item as a turn start.
#[tokio::test(flavor = "current_thread")]
async fn rewind_removes_turn_after_synthetic_auto_wake_unmarked() {
    let local = tokio::task::LocalSet::new();
    local.run_until(run_rewind_over_synthetic_turn(false)).await;
}

/// Marked items (what `turn.rs` stamps on every turn start): the explicit
/// per-item prompt index takes priority.
#[tokio::test(flavor = "current_thread")]
async fn rewind_removes_turn_after_synthetic_auto_wake_marked() {
    let local = tokio::task::LocalSet::new();
    local.run_until(run_rewind_over_synthetic_turn(true)).await;
}

/// Rewind on a session with no prompts: the picker has nothing to offer and
/// an execute request is rejected (no silent no-op "success").
#[tokio::test(flavor = "current_thread")]
async fn rewind_with_no_prompts_lists_no_points_and_rejects_execute() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            let actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

            let points = actor.get_rewind_points().await;
            assert!(
                points.rewind_points.is_empty(),
                "fresh session must expose zero rewind points: {points:?}"
            );

            let resp = actor
                .handle_rewind(RewindRequest {
                    target_prompt_index: 0,
                    force: true,
                    file_guard: None,
                    mode: RewindMode::ConversationOnly,
                })
                .await
                .expect("handle_rewind ok");
            assert!(!resp.success, "rewind with no prompts must be rejected");
            assert!(
                resp.error
                    .as_deref()
                    .unwrap_or("")
                    .contains("Cannot rewind"),
                "rejection must carry a clear error: {resp:?}"
            );
        })
        .await;
}

/// Rewind to the start of the conversation (target = 0) keeps only the
/// session preamble — System + user_info + pre-turn synthetic reminders —
/// even when turn 0 exists alongside synthetic auto-wake turns.
#[tokio::test(flavor = "current_thread")]
async fn rewind_to_start_keeps_only_preamble() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            let actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

            let mut conversation = seed_conversation(true);
            // Pre-turn reminder in the preamble prefix must survive target=0.
            conversation.insert(2, ConversationItem::system_reminder("skills"));
            let mut snap = actor
                .chat_state_handle
                .snapshot()
                .await
                .expect("snapshot available");
            snap.conversation = conversation;
            snap.prompt_index = 3;
            snap.prompt_texts = vec![
                "P0".into(),
                "Background task abc completed".into(),
                "P2".into(),
            ];
            actor.chat_state_handle.restore_snapshot(snap);

            let resp = actor
                .handle_rewind(RewindRequest {
                    target_prompt_index: 0,
                    force: true,
                    file_guard: None,
                    mode: RewindMode::ConversationOnly,
                })
                .await
                .expect("handle_rewind ok");
            assert!(resp.success, "rewind to start should succeed: {resp:?}");
            assert_eq!(resp.prompt_text.as_deref(), Some("P0"));

            let conv = actor.chat_state_handle.get_conversation().await;
            let texts: Vec<String> = conv.iter().map(|c| c.text_content()).collect();
            assert_eq!(
                texts,
                vec!["SYS", "<user_info>OS: test</user_info>", "skills"],
                "target 0 must keep only the preamble prefix"
            );
            assert_eq!(actor.chat_state_handle.get_prompt_index().await, 0);

            // With prompt_index back at 0 the session behaves like a fresh
            // one: no points, further rewinds rejected.
            assert!(actor.get_rewind_points().await.rewind_points.is_empty());
            let again = actor
                .handle_rewind(RewindRequest {
                    target_prompt_index: 0,
                    force: true,
                    file_guard: None,
                    mode: RewindMode::ConversationOnly,
                })
                .await
                .expect("handle_rewind ok");
            assert!(!again.success, "no prompts left to rewind: {again:?}");
        })
        .await;
}

/// Two sequential rewinds narrow the history correctly each time — the
/// second rewind operates on the already-truncated conversation (markers
/// still present on the surviving items).
#[tokio::test(flavor = "current_thread")]
async fn rewind_twice_narrows_history_each_time() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            let actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

            // 5 turns: real, wake, real, wake, real.
            let marked = |text: &str, idx: usize| {
                let mut item = ConversationItem::user(text);
                item.set_prompt_index(idx);
                item
            };
            let marked_wake = |text: &str, idx: usize| {
                let mut item = ConversationItem::task_completed(text);
                item.set_prompt_index(idx);
                item
            };
            let mut snap = actor
                .chat_state_handle
                .snapshot()
                .await
                .expect("snapshot available");
            snap.conversation = vec![
                ConversationItem::system("SYS"),
                ConversationItem::user("<user_info>OS: test</user_info>"),
                marked("P0", 0),
                ConversationItem::assistant("A0"),
                marked_wake("W1", 1),
                ConversationItem::assistant("A1"),
                marked("P2", 2),
                ConversationItem::assistant("A2"),
                marked_wake("W3", 3),
                ConversationItem::assistant("A3"),
                marked("P4", 4),
                ConversationItem::assistant("A4"),
            ];
            snap.prompt_index = 5;
            snap.prompt_texts = vec![
                "P0".into(),
                "W1".into(),
                "P2".into(),
                "W3".into(),
                "P4".into(),
            ];
            actor.chat_state_handle.restore_snapshot(snap);

            // First rewind: to turn 3 (drops W3, A3, P4, A4).
            let first = actor
                .handle_rewind(RewindRequest {
                    target_prompt_index: 3,
                    force: true,
                    file_guard: None,
                    mode: RewindMode::ConversationOnly,
                })
                .await
                .expect("handle_rewind ok");
            assert!(first.success, "{first:?}");
            assert_eq!(first.prompt_text.as_deref(), Some("W3"));
            let conv = actor.chat_state_handle.get_conversation().await;
            let texts: Vec<String> = conv.iter().map(|c| c.text_content()).collect();
            assert_eq!(
                texts,
                vec![
                    "SYS",
                    "<user_info>OS: test</user_info>",
                    "P0",
                    "A0",
                    "W1",
                    "A1",
                    "P2",
                    "A2",
                ],
                "first rewind keeps turns 0..=2"
            );
            assert_eq!(actor.chat_state_handle.get_prompt_index().await, 3);

            // Second rewind: to turn 1 (drops W1, A1, P2, A2).
            let second = actor
                .handle_rewind(RewindRequest {
                    target_prompt_index: 1,
                    force: true,
                    file_guard: None,
                    mode: RewindMode::ConversationOnly,
                })
                .await
                .expect("handle_rewind ok");
            assert!(second.success, "{second:?}");
            assert_eq!(second.prompt_text.as_deref(), Some("W1"));
            let conv = actor.chat_state_handle.get_conversation().await;
            let texts: Vec<String> = conv.iter().map(|c| c.text_content()).collect();
            assert_eq!(
                texts,
                vec!["SYS", "<user_info>OS: test</user_info>", "P0", "A0"],
                "second rewind keeps only turn 0"
            );
            assert_eq!(actor.chat_state_handle.get_prompt_index().await, 1);

            // Picker after two rewinds offers exactly turn 0.
            let points = actor.get_rewind_points().await;
            let indices: Vec<usize> = points
                .rewind_points
                .iter()
                .map(|p| p.prompt_index)
                .collect();
            assert_eq!(indices, vec![0]);
        })
        .await;
}

/// Midpoint rewind with synthetic turns on BOTH sides of the cut, in both
/// marker and counting-fallback modes.
#[tokio::test(flavor = "current_thread")]
async fn rewind_to_midpoint_with_synthetic_turns_on_both_sides() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            for mark_turn_starts in [false, true] {
                let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
                let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
                let actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

                let user = |text: &str, idx: usize| {
                    let mut item = ConversationItem::user(text);
                    if mark_turn_starts {
                        item.set_prompt_index(idx);
                    }
                    item
                };
                let wake = |text: &str, idx: usize| {
                    let mut item = ConversationItem::task_completed(text);
                    if mark_turn_starts {
                        item.set_prompt_index(idx);
                    }
                    item
                };
                let mut snap = actor
                    .chat_state_handle
                    .snapshot()
                    .await
                    .expect("snapshot available");
                snap.conversation = vec![
                    ConversationItem::system("SYS"),
                    ConversationItem::user("<user_info>OS: test</user_info>"),
                    user("P0", 0),
                    ConversationItem::assistant("A0"),
                    wake("W1", 1),
                    ConversationItem::assistant("A1"),
                    user("P2", 2),
                    ConversationItem::assistant("A2"),
                    wake("W3", 3),
                    ConversationItem::assistant("A3"),
                    user("P4", 4),
                    ConversationItem::assistant("A4"),
                ];
                snap.prompt_index = 5;
                snap.prompt_texts = vec![
                    "P0".into(),
                    "W1".into(),
                    "P2".into(),
                    "W3".into(),
                    "P4".into(),
                ];
                actor.chat_state_handle.restore_snapshot(snap);

                let resp = actor
                    .handle_rewind(RewindRequest {
                        target_prompt_index: 2,
                        force: true,
                        file_guard: None,
                        mode: RewindMode::ConversationOnly,
                    })
                    .await
                    .expect("handle_rewind ok");
                assert!(resp.success, "mark={mark_turn_starts}: {resp:?}");
                assert_eq!(resp.prompt_text.as_deref(), Some("P2"));

                let conv = actor.chat_state_handle.get_conversation().await;
                let texts: Vec<String> = conv.iter().map(|c| c.text_content()).collect();
                assert_eq!(
                    texts,
                    vec![
                        "SYS",
                        "<user_info>OS: test</user_info>",
                        "P0",
                        "A0",
                        "W1",
                        "A1",
                    ],
                    "midpoint rewind keeps turns 0..=1 (mark={mark_turn_starts})"
                );
                assert_eq!(actor.chat_state_handle.get_prompt_index().await, 2);
            }
        })
        .await;
}

/// Rewind to the auto-wake turn itself (target = 1) must cut the auto-wake
/// item and everything after it.
#[tokio::test(flavor = "current_thread")]
async fn rewind_to_synthetic_auto_wake_turn_cuts_at_the_wake() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            let actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

            let mut snap = actor
                .chat_state_handle
                .snapshot()
                .await
                .expect("snapshot available");
            snap.conversation = seed_conversation(true);
            snap.prompt_index = 3;
            snap.prompt_texts = vec![
                "P0".into(),
                "Background task abc completed".into(),
                "P2".into(),
            ];
            actor.chat_state_handle.restore_snapshot(snap);

            let resp = actor
                .handle_rewind(RewindRequest {
                    target_prompt_index: 1,
                    force: true,
                    file_guard: None,
                    mode: RewindMode::ConversationOnly,
                })
                .await
                .expect("handle_rewind ok");
            assert!(resp.success, "rewind should succeed: {resp:?}");

            let conv = actor.chat_state_handle.get_conversation().await;
            let texts: Vec<String> = conv.iter().map(|c| c.text_content()).collect();
            assert_eq!(
                texts,
                vec!["SYS", "<user_info>OS: test</user_info>", "P0", "A0"],
                "auto-wake turn and everything after it must be removed"
            );
            assert_eq!(actor.chat_state_handle.get_prompt_index().await, 1);
        })
        .await;
}
