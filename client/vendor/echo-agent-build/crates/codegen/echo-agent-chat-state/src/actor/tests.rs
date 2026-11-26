//! Tests for ChatStateActor.

use std::num::NonZeroU64;
use std::time::Duration;

use echo_agent_sampling_types::{ConversationItem, SamplingConfig};
use tokio::sync::mpsc;

use crate::StrictAppendAck;
use crate::actor::ChatStateActor;
use crate::events::ChatStateEvent;
use crate::persistence::{MockChatPersistence, MockPersistenceReceiver, PersistenceRecord};

/// Helper to build a `SamplingConfig` for tests.
fn test_config() -> SamplingConfig {
    test_config_with_window(128_000)
}

fn test_config_with_window(context_window: u64) -> SamplingConfig {
    SamplingConfig {
        base_url: "https://api.example.com".to_string(),
        model: "test-model".to_string(),
        max_completion_tokens: None,
        temperature: None,
        top_p: None,
        api_backend: Default::default(),
        extra_headers: Default::default(),
        query_params: Default::default(),
        env_http_headers: Default::default(),
        context_window: NonZeroU64::new(context_window)
            .expect("test context_window must be non-zero"),
        reasoning_effort: None,
        stream_tool_calls: None,
        supports_tool_calls: None,
    }
}

/// Test harness that spawns an actor and keeps the event + persistence channels.
struct TestHarness {
    handle: crate::handle::ChatStateHandle,
    event_rx: mpsc::UnboundedReceiver<ChatStateEvent>,
    persistence_rx: MockPersistenceReceiver,
    _cancellation_token: tokio_util::sync::CancellationToken,
}

impl TestHarness {
    fn new() -> Self {
        Self::with_config(vec![], test_config())
    }

    fn with_conversation(items: Vec<ConversationItem>) -> Self {
        Self::with_config(items, test_config())
    }

    fn with_context_window(window: u64) -> Self {
        Self::with_config(vec![], test_config_with_window(window))
    }

    fn with_config(items: Vec<ConversationItem>, config: SamplingConfig) -> Self {
        let (mock, persistence_rx) = MockChatPersistence::new();
        Self::with_persistence(items, config, mock, persistence_rx)
    }

    fn with_manual_persistence_ack(items: Vec<ConversationItem>) -> Self {
        let (mock, persistence_rx) = MockChatPersistence::new_with_manual_persistence_ack();
        Self::with_persistence(items, test_config(), mock, persistence_rx)
    }

    fn with_persistence(
        items: Vec<ConversationItem>,
        config: SamplingConfig,
        mock: MockChatPersistence,
        persistence_rx: MockPersistenceReceiver,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let token = tokio_util::sync::CancellationToken::new();
        let handle = ChatStateActor::spawn(items, config, Box::new(mock), event_tx, token.clone());
        Self {
            handle,
            event_rx,
            persistence_rx,
            _cancellation_token: token,
        }
    }

    /// Drain the next event from the event channel (with timeout).
    async fn next_event(&mut self) -> ChatStateEvent {
        tokio::time::timeout(Duration::from_secs(1), self.event_rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("event channel closed")
    }

    /// Drain all pending events.
    fn drain_events(&mut self) -> Vec<ChatStateEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// Drain all pending persistence records.
    fn drain_persistence(&mut self) -> Vec<PersistenceRecord> {
        self.persistence_rx.drain()
    }
}

// ============================================================================
// Lifecycle tests
// ============================================================================

#[tokio::test]
async fn actor_spawns_and_shuts_down_via_cancellation() {
    let (mock, _rx) = MockChatPersistence::new();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let token = tokio_util::sync::CancellationToken::new();
    let _handle = ChatStateActor::spawn(
        vec![],
        test_config(),
        Box::new(mock),
        event_tx,
        token.clone(),
    );
    token.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn actor_shuts_down_when_all_handles_dropped() {
    let (mock, _rx) = MockChatPersistence::new();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let token = tokio_util::sync::CancellationToken::new();
    let handle = ChatStateActor::spawn(vec![], test_config(), Box::new(mock), event_tx, token);
    drop(handle);
    tokio::time::sleep(Duration::from_millis(50)).await;
}

// ============================================================================
// Mutation tests
// ============================================================================

#[tokio::test]
async fn push_user_message_appends_and_persists() {
    let mut h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("hello"));

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 1);

    let records = h.drain_persistence();
    assert_eq!(records.len(), 1);
    assert!(matches!(&records[0], PersistenceRecord::Message(_)));
}

#[tokio::test]
async fn push_user_message_and_ack_waits_for_actor_acceptance() {
    let mut h = TestHarness::new();
    let ack = h
        .handle
        .push_user_message_and_ack(ConversationItem::user("hello"))
        .await;

    assert!(ack.is_some());

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 1);

    let records = h.drain_persistence();
    assert_eq!(records.len(), 1);
    assert!(matches!(&records[0], PersistenceRecord::Message(_)));
}

#[tokio::test]
async fn strict_switch_append_preserves_prefix_and_deduplicates_generation() {
    let prefix = vec![
        ConversationItem::system("sys"),
        ConversationItem::assistant("assistant"),
        ConversationItem::tool_result("dangling", "must remain"),
    ];
    let prefix_json: Vec<Vec<u8>> = prefix
        .iter()
        .map(|item| serde_json::to_vec(item).unwrap())
        .collect();
    let mut h = TestHarness::with_conversation(prefix);
    let reminder = ConversationItem::working_directory_switch("moved", 3);

    assert!(matches!(
        h.handle
            .append_working_directory_switch_and_ack(
                "moved".into(),
                std::num::NonZeroU64::new(3).unwrap(),
            )
            .await
            .unwrap(),
        StrictAppendAck::Appended
    ));
    let conversation = h.handle.get_conversation().await;
    assert_eq!(conversation.len(), 4);
    for (actual, expected) in conversation.iter().zip(&prefix_json) {
        assert_eq!(serde_json::to_vec(actual).unwrap(), *expected);
    }
    assert_eq!(
        serde_json::to_vec(&conversation[3]).unwrap(),
        serde_json::to_vec(&reminder).unwrap()
    );
    assert!(matches!(
        h.drain_persistence().as_slice(),
        [PersistenceRecord::AcknowledgedMessage(_)]
    ));

    assert!(matches!(
        h.handle
            .append_working_directory_switch_and_ack(
                "different text".into(),
                std::num::NonZeroU64::new(3).unwrap(),
            )
            .await
            .unwrap(),
        StrictAppendAck::AlreadyPresent(_)
    ));
    assert_eq!(h.handle.get_conversation().await.len(), 4);
    assert!(matches!(
        h.drain_persistence().as_slice(),
        [PersistenceRecord::AcknowledgedMessage(_)]
    ));

    assert!(matches!(
        h.handle
            .append_working_directory_switch_and_ack(
                "next move".into(),
                std::num::NonZeroU64::new(4).unwrap(),
            )
            .await
            .unwrap(),
        StrictAppendAck::Appended
    ));
    assert_eq!(h.handle.get_conversation().await.len(), 5);
}

#[tokio::test]
async fn strict_switch_append_ack_waits_for_persistence() {
    let mut h = TestHarness::with_manual_persistence_ack(vec![]);
    let handle = h.handle.clone();
    let task = tokio::spawn(async move {
        handle
            .append_working_directory_switch_and_ack(
                "moved".into(),
                std::num::NonZeroU64::new(1).unwrap(),
            )
            .await
    });
    let persistence_ack = h
        .persistence_rx
        .next_persistence_ack()
        .await
        .expect("acknowledged append requested");
    assert!(matches!(
        h.drain_persistence().as_slice(),
        [PersistenceRecord::AcknowledgedMessage(_)]
    ));
    assert!(!task.is_finished(), "actor ack must wait for persistence");
    persistence_ack.send(Ok(StrictAppendAck::Appended)).unwrap();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        StrictAppendAck::Appended
    ));
}

#[tokio::test]
async fn committed_storage_result_converges_actor_memory() {
    let mut h = TestHarness::with_manual_persistence_ack(vec![]);
    let handle = h.handle.clone();
    let task = tokio::spawn(async move {
        handle
            .append_working_directory_switch_and_ack(
                "moved".into(),
                std::num::NonZeroU64::new(2).unwrap(),
            )
            .await
    });
    let persistence_ack = h.persistence_rx.next_persistence_ack().await.unwrap();
    persistence_ack
        .send(Err(crate::StrictAppendError::Committed {
            acknowledgement: StrictAppendAck::Appended,
            source: std::io::Error::other("summary failed"),
        }))
        .unwrap();
    let result = task.await.unwrap();
    assert!(matches!(
        result,
        Err(crate::StrictAppendError::Committed {
            acknowledgement: StrictAppendAck::Appended,
            ..
        })
    ));
    let conversation = h.handle.get_conversation().await;
    assert_eq!(conversation.len(), 1);
    assert_eq!(
        conversation[0].working_directory_switch_generation(),
        Some(2)
    );
}

#[tokio::test]
async fn already_present_replaces_stale_switch_in_actor_memory() {
    let generation = NonZeroU64::new(3).unwrap();
    let mut h =
        TestHarness::with_manual_persistence_ack(vec![ConversationItem::working_directory_switch(
            "stale",
            generation.get(),
        )]);
    let handle = h.handle.clone();
    let task = tokio::spawn(async move {
        handle
            .append_working_directory_switch_and_ack("candidate".into(), generation)
            .await
    });
    h.persistence_rx
        .next_persistence_ack()
        .await
        .unwrap()
        .send(Ok(StrictAppendAck::AlreadyPresent(
            ConversationItem::working_directory_switch("authoritative", generation.get()),
        )))
        .unwrap();

    assert!(matches!(
        task.await.unwrap().unwrap(),
        StrictAppendAck::AlreadyPresent(item) if item.text_content() == "authoritative"
    ));
    let conversation = h.handle.get_conversation().await;
    assert_eq!(conversation.len(), 1);
    assert_eq!(conversation[0].text_content(), "authoritative");
}

#[tokio::test]
async fn committed_already_present_replaces_retry_candidate_in_actor_memory() {
    let generation = NonZeroU64::new(4).unwrap();
    let mut h =
        TestHarness::with_manual_persistence_ack(vec![ConversationItem::working_directory_switch(
            "retry candidate",
            generation.get(),
        )]);
    let handle = h.handle.clone();
    let task = tokio::spawn(async move {
        handle
            .append_working_directory_switch_and_ack("another retry".into(), generation)
            .await
    });
    h.persistence_rx
        .next_persistence_ack()
        .await
        .unwrap()
        .send(Err(crate::StrictAppendError::Committed {
            acknowledgement: StrictAppendAck::AlreadyPresent(
                ConversationItem::working_directory_switch("authoritative", generation.get()),
            ),
            source: std::io::Error::other("summary failed"),
        }))
        .unwrap();

    assert!(matches!(
        task.await.unwrap(),
        Err(crate::StrictAppendError::Committed {
            acknowledgement: StrictAppendAck::AlreadyPresent(item),
            ..
        }) if item.text_content() == "authoritative"
    ));
    let conversation = h.handle.get_conversation().await;
    assert_eq!(conversation.len(), 1);
    assert_eq!(conversation[0].text_content(), "authoritative");
}

#[tokio::test]
async fn dropped_storage_reply_is_indeterminate_and_leaves_memory_unchanged() {
    let mut h = TestHarness::with_manual_persistence_ack(vec![]);
    let handle = h.handle.clone();
    let task = tokio::spawn(async move {
        handle
            .append_working_directory_switch_and_ack(
                "moved".into(),
                std::num::NonZeroU64::new(2).unwrap(),
            )
            .await
    });
    drop(h.persistence_rx.next_persistence_ack().await.unwrap());
    assert!(matches!(
        task.await.unwrap(),
        Err(crate::StrictAppendError::Indeterminate(_))
    ));
    assert!(h.handle.get_conversation().await.is_empty());
}

#[tokio::test]
async fn uncommitted_storage_error_leaves_actor_memory_unchanged() {
    let mut h = TestHarness::with_manual_persistence_ack(vec![]);
    let handle = h.handle.clone();
    let task = tokio::spawn(async move {
        handle
            .append_working_directory_switch_and_ack(
                "moved".into(),
                std::num::NonZeroU64::new(2).unwrap(),
            )
            .await
    });
    let persistence_ack = h.persistence_rx.next_persistence_ack().await.unwrap();
    persistence_ack
        .send(Err(crate::StrictAppendError::NotCommitted(
            std::io::Error::other("append failed"),
        )))
        .unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(h.handle.get_conversation().await.is_empty());
}

#[tokio::test]
async fn push_assistant_response_appends_and_persists() {
    let mut h = TestHarness::new();
    h.handle
        .push_assistant_response(ConversationItem::assistant("hi"));

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 1);

    let records = h.drain_persistence();
    assert_eq!(records.len(), 1);
}

#[tokio::test]
async fn push_tool_result_appends_and_persists() {
    let mut h = TestHarness::new();
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-1", "result"));

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 1);

    let records = h.drain_persistence();
    assert_eq!(records.len(), 1);
}

#[tokio::test]
async fn record_token_usage_emits_event() {
    let mut h = TestHarness::new();
    h.handle.record_token_usage(1000);
    let event = h.next_event().await;
    assert!(matches!(
        event,
        ChatStateEvent::TokensUpdated { total_tokens: 1000 }
    ));

    let tokens = h.handle.get_total_tokens().await;
    assert_eq!(tokens, 1000);
}

#[tokio::test]
async fn record_last_turn_usage_round_trip() {
    use echo_agent_sampling_types::TokenUsage;

    let h = TestHarness::new();

    // Initial state: nothing stashed.
    assert!(h.handle.get_last_turn_usage().await.is_none());

    let usage = TokenUsage {
        prompt_tokens: 1234,
        completion_tokens: 56,
        total_tokens: 1290,
        reasoning_tokens: 0,
        cached_prompt_tokens: 800,
        cache_creation_prompt_tokens: 0,
    };
    h.handle.record_last_turn_usage(usage.clone());

    let got = h.handle.get_last_turn_usage().await.expect("usage stashed");
    assert_eq!(got.prompt_tokens, 1234);
    assert_eq!(got.completion_tokens, 56);
    assert_eq!(got.cached_prompt_tokens, 800);

    // Subsequent record overwrites.
    let next = TokenUsage {
        prompt_tokens: 9999,
        completion_tokens: 1,
        total_tokens: 10000,
        reasoning_tokens: 0,
        cached_prompt_tokens: 0,
        cache_creation_prompt_tokens: 0,
    };
    h.handle.record_last_turn_usage(next);
    let got2 = h
        .handle
        .get_last_turn_usage()
        .await
        .expect("overwritten usage");
    assert_eq!(got2.prompt_tokens, 9999);
    assert_eq!(got2.cached_prompt_tokens, 0);
}

#[tokio::test]
async fn prompt_usage_ledger_via_handle_resets_and_clears() {
    use echo_agent_sampling_types::TokenUsage;

    let call = TokenUsage {
        prompt_tokens: 10,
        completion_tokens: 2,
        total_tokens: 12,
        reasoning_tokens: 0,
        cached_prompt_tokens: 0,
        cache_creation_prompt_tokens: 0,
    };

    let h = TestHarness::new();
    h.handle
        .record_model_call_usage(Some("m".into()), call.clone(), None, None);
    h.handle
        .record_model_call_usage(None, call.clone(), None, None);
    assert_eq!(
        h.handle
            .try_get_prompt_usage()
            .await
            .unwrap()
            .expect("prompt ledger")
            .totals
            .model_calls,
        2
    );
    h.handle.increment_prompt_index();
    assert!(
        h.handle
            .try_get_prompt_usage()
            .await
            .ok()
            .flatten()
            .is_none()
    );
    assert_eq!(
        h.handle
            .try_get_session_usage()
            .await
            .expect("actor alive")
            .totals
            .model_calls,
        2
    );

    h.handle
        .record_model_call_usage(Some("m".into()), call.clone(), None, None);
    let snap = h.handle.snapshot().await.unwrap();
    h.handle.restore_snapshot(snap);
    assert!(
        h.handle
            .try_get_prompt_usage()
            .await
            .ok()
            .flatten()
            .is_none()
    );

    h.handle
        .record_model_call_usage(Some("m".into()), call, None, None);
    h.handle.truncate_to_prompt_index(0).await;
    assert!(
        h.handle
            .try_get_prompt_usage()
            .await
            .ok()
            .flatten()
            .is_none()
    );
}

#[tokio::test]
async fn estimated_tokens_tracks_tool_result_delta() {
    let h = TestHarness::new();
    h.handle.record_token_usage(100_000);

    // Push a tool result with 4000 chars → ~1000 estimated tokens
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-1", "x".repeat(4000)));

    let estimated = h.handle.get_estimated_total_tokens().await;
    assert_eq!(estimated, 101_000); // 100K model-reported + 1K delta

    // model-reported total_tokens is unchanged
    let actual = h.handle.get_total_tokens().await;
    assert_eq!(actual, 100_000);
}

#[tokio::test]
async fn estimated_tokens_resets_on_model_response() {
    let h = TestHarness::new();
    h.handle.record_token_usage(100_000);
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-1", "x".repeat(4000)));

    assert_eq!(h.handle.get_estimated_total_tokens().await, 101_000);

    // Model responds with a new total — delta resets
    h.handle.record_token_usage(105_000);
    assert_eq!(h.handle.get_estimated_total_tokens().await, 105_000);
    assert_eq!(h.handle.get_total_tokens().await, 105_000);
}

#[tokio::test]
async fn estimated_tokens_tracks_synthetic_user_message_delta() {
    let h = TestHarness::new();
    h.handle.record_token_usage(100_000);

    // Simulate a 4MB background-task completion notification pushed as a
    // synthetic User item between turns.
    h.handle.push_user_message(ConversationItem::user(
        "Background task completed:\n".to_string() + &"x".repeat(4_000_000),
    ));

    let estimated = h.handle.get_estimated_total_tokens().await;
    // 100K model-reported + ~1M tokens for the 4MB reminder.
    assert!(
        (1_099_000..=1_101_000).contains(&estimated),
        "expected ~1.1M tokens estimated, got {estimated}",
    );

    // model-reported `total_tokens` is unchanged — only the delta moved.
    assert_eq!(h.handle.get_total_tokens().await, 100_000);
}

/// Regression: a normal user prompt pushed at turn start must increment
/// the delta. The bump is voided when the model responds and
/// `record_token_usage` includes the prompt in `usage.total_tokens`,
/// keeping the post-response total accurate.
#[tokio::test]
async fn estimated_tokens_tracks_real_user_message_and_resets_on_response() {
    let h = TestHarness::new();
    h.handle.record_token_usage(100_000);

    // Real user turn — 4000 chars / 4 = ~1000 tokens.
    h.handle
        .push_user_message(ConversationItem::user("u".repeat(4000)));

    let pre = h.handle.get_estimated_total_tokens().await;
    assert!(
        (101_000..=101_010).contains(&pre),
        "expected ~101K after user push, got {pre}",
    );

    // Model responds; the API's `usage.total_tokens` already includes
    // the new user message. Delta must reset to zero.
    h.handle.record_token_usage(103_000);
    assert_eq!(h.handle.get_estimated_total_tokens().await, 103_000);
    assert_eq!(h.handle.get_total_tokens().await, 103_000);
}

/// Regression: pushing the assistant response back into the chat
/// must NOT bump the delta — the model already counted it in
/// `usage.completion_tokens` (which is folded into the just-applied
/// `total_tokens`). Bumping again would double-count the assistant.
#[tokio::test]
async fn assistant_response_push_does_not_bump_estimated_delta() {
    let h = TestHarness::new();
    h.handle.record_token_usage(100_000);
    h.handle
        .push_assistant_response(ConversationItem::assistant("a".repeat(4000)));

    let estimated = h.handle.get_estimated_total_tokens().await;
    assert_eq!(
        estimated, 100_000,
        "assistant push must not increment the delta (already in `usage.total_tokens`)"
    );
}

#[tokio::test]
async fn provider_counted_model_output_persists_without_bumping_estimate() {
    let h = TestHarness::new();
    h.handle.record_token_usage(100_000);
    h.handle.push_model_output(ConversationItem::Reasoning(
        echo_agent_sampling_types::synthesized_reasoning_item("r".repeat(4_000)),
    ));

    assert!(matches!(
        h.handle.get_conversation().await.as_slice(),
        [ConversationItem::Reasoning(_)]
    ));
    assert_eq!(h.handle.get_estimated_total_tokens().await, 100_000);
}

#[tokio::test]
async fn estimated_tokens_resets_on_truncate() {
    let mut h = TestHarness::new();
    h.handle.record_token_usage(100_000);
    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle.increment_prompt_index();
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-1", "x".repeat(4000)));

    assert_eq!(h.handle.get_estimated_total_tokens().await, 101_000);

    // Rewind removes the tool result — delta must reset
    h.drain_events();
    h.handle.truncate_to_prompt_index(0).await;
    let post_rewind = h.handle.get_estimated_total_tokens().await;
    assert!(
        post_rewind < 100_000,
        "post-rewind tokens should reflect truncated conversation, not stale model count"
    );
}

#[tokio::test]
async fn increment_prompt_index_emits_event() {
    let mut h = TestHarness::new();
    h.handle.increment_prompt_index();
    let event = h.next_event().await;
    assert!(matches!(
        event,
        ChatStateEvent::PromptIndexChanged { new_index: 1 }
    ));

    let idx = h.handle.get_prompt_index().await;
    assert_eq!(idx, 1);
}

#[tokio::test]
async fn replace_conversation_persists_and_emits_reset() {
    let mut h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("a"));
    h.handle.push_user_message(ConversationItem::user("b"));

    // Drain the two Message records
    let _ = h.handle.get_conversation().await; // sync point
    h.drain_persistence();

    let new_items = vec![ConversationItem::system("compacted")];
    h.handle.replace_conversation(new_items);

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 1);

    let records = h.drain_persistence();
    assert_eq!(records.len(), 1);
    assert!(matches!(&records[0], PersistenceRecord::ReplaceHistory(_)));
}

#[tokio::test]
async fn strip_conversation_images_replaces_only_listed_urls_and_persists() {
    let mut user = match ConversationItem::user("look at this") {
        ConversationItem::User(u) => u,
        _ => unreachable!(),
    };
    user.add_image("data:image/png;base64,AAAA");
    let mut later = match ConversationItem::user("and this one") {
        ConversationItem::User(u) => u,
        _ => unreachable!(),
    };
    later.add_image("data:image/png;base64,BBBB");
    let mut h = TestHarness::with_conversation(vec![
        ConversationItem::User(user),
        ConversationItem::User(later),
    ]);

    let outcome = h
        .handle
        .strip_conversation_images(vec!["data:image/png;base64,AAAA".into()])
        .await;
    assert_eq!(
        outcome,
        crate::StripOutcome::Applied { stripped: 1 },
        "ack must carry the disk-applied count"
    );

    let conv = h.handle.get_conversation().await; // sync point
    assert_eq!(
        conv.len(),
        2,
        "in-place strip must not add or remove conversation items"
    );
    let ConversationItem::User(u) = &conv[0] else {
        panic!("expected user item");
    };
    assert!(
        u.content
            .iter()
            .all(|p| !matches!(p, echo_agent_sampling_types::ContentPart::Image { .. })),
        "listed image part must be replaced"
    );
    let ConversationItem::User(survivor) = &conv[1] else {
        panic!("expected user item");
    };
    assert!(
        survivor
            .content
            .iter()
            .any(|p| matches!(p, echo_agent_sampling_types::ContentPart::Image { .. })),
        "unlisted image must survive the scoped strip"
    );

    let records = h.drain_persistence();
    // The recoverability contract: strip rewrites go through the single
    // backup-gated flavor, never the plain, unguarded ReplaceHistory.
    assert!(
        records
            .iter()
            .any(|r| matches!(r, PersistenceRecord::ReplaceHistoryForStrip(_))),
        "strip must persist via the backup-gated flavor, got {records:?}"
    );
    assert!(
        !records
            .iter()
            .any(|r| matches!(r, PersistenceRecord::ReplaceHistory(_))),
        "strip must not use the unguarded replace, got {records:?}"
    );
}

#[tokio::test]
async fn strip_conversation_images_does_not_reseed_provider_tokens() {
    let mut user = match ConversationItem::user("look at this") {
        ConversationItem::User(u) => u,
        _ => unreachable!(),
    };
    user.add_image("data:image/png;base64,AAAA");
    let mut h = TestHarness::with_conversation(vec![ConversationItem::User(user)]);
    h.handle.record_token_usage(100_000);
    let _ = h.handle.get_total_tokens().await;
    h.drain_events();

    let outcome = h
        .handle
        .strip_conversation_images(vec!["data:image/png;base64,AAAA".into()])
        .await;
    assert_eq!(
        outcome,
        crate::StripOutcome::Applied { stripped: 1 },
        "ack must carry the disk-applied count"
    );

    assert_eq!(
        h.handle.get_total_tokens().await,
        100_000,
        "provider total must survive a surgical strip"
    );
    assert!(
        !h.drain_events()
            .iter()
            .any(|e| matches!(e, ChatStateEvent::ConversationReset { .. })),
        "strip must not emit ConversationReset"
    );
}

/// The honest-failure half of the contract: a failed disk write must
/// surface as WriteFailed (never as Applied), so nobody tells the user a
/// still-poisoned file was cleaned.
#[tokio::test]
async fn strip_conversation_images_reports_write_failure() {
    let mut user = match ConversationItem::user("look at this") {
        ConversationItem::User(u) => u,
        _ => unreachable!(),
    };
    user.add_image("data:image/png;base64,AAAA");
    let (mock, persistence_rx) = MockChatPersistence::new_failing_strip_writes();
    let h = TestHarness::with_persistence(
        vec![ConversationItem::User(user)],
        test_config(),
        mock,
        persistence_rx,
    );

    let outcome = h
        .handle
        .strip_conversation_images(vec!["data:image/png;base64,AAAA".into()])
        .await;
    assert_eq!(
        outcome,
        crate::StripOutcome::WriteFailed { stripped: 1 },
        "a failed disk write must never read as Applied"
    );
}

#[tokio::test]
async fn strip_conversation_images_is_a_noop_without_images() {
    let mut h = TestHarness::with_conversation(vec![ConversationItem::user("plain text")]);

    let outcome = h
        .handle
        .strip_conversation_images(vec!["data:image/png;base64,AAAA".into()])
        .await;
    assert_eq!(
        outcome,
        crate::StripOutcome::NoMatch,
        "no match must be typed, not a fake success"
    );

    let conv = h.handle.get_conversation().await; // sync point
    assert_eq!(conv.len(), 1);
    assert!(
        h.drain_persistence().is_empty(),
        "no images stripped must mean no persistence write"
    );
}

#[tokio::test]
async fn compaction_reseed_carries_provider_overhead() {
    let h = TestHarness::new();
    // ~1k estimated tokens; provider reports 51k → 50k overhead.
    h.handle
        .push_user_message(ConversationItem::user("x".repeat(4000)));
    h.handle.record_token_usage(51_000);

    let compacted = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("summary ".repeat(500)),
    ];
    h.handle.replace_conversation_for_compaction(compacted);

    let total = h.handle.get_total_tokens().await;
    let estimate_only = 1_000u64;
    assert!(
        total > estimate_only * 10,
        "reseed must carry provider overhead, got {total}"
    );
    assert!(
        total <= 51_000,
        "reseed must never exceed the pre-compaction provider total, got {total}"
    );
}

#[tokio::test]
async fn compaction_reseed_scales_overhead_down_with_deleted_content() {
    let h = TestHarness::new();
    h.handle
        .push_user_message(ConversationItem::user("x".repeat(160_000)));
    let estimate_at_response =
        crate::estimate_conversation_tokens(&h.handle.get_conversation().await);
    assert_eq!(estimate_at_response, 40_000);
    let provider_total = 87_000;
    h.handle.record_token_usage(provider_total);

    let compacted = vec![ConversationItem::user("z".repeat(12_000))];
    let compacted_estimate = crate::estimate_conversation_tokens(&compacted);
    assert_eq!(compacted_estimate, 3_000);
    h.handle.replace_conversation_for_compaction(compacted);

    let total = h.handle.get_total_tokens().await;
    assert_eq!(total, 6_525, "expected ratio-scaled overhead, got {total}");
    let additive_model = compacted_estimate + (provider_total - estimate_at_response);
    assert_eq!(additive_model, 50_000);
    assert!(
        total < additive_model / 4,
        "ratio reseed ({total}) must be far below the additive over-count ({additive_model})"
    );
    assert!(
        total > compacted_estimate,
        "some overhead must still be carried"
    );
}

#[tokio::test]
async fn compaction_reseed_excludes_post_response_deltas_from_overhead() {
    let h = TestHarness::new();
    h.handle
        .push_user_message(ConversationItem::user("y".repeat(4000)));
    h.handle.record_token_usage(11_000);
    h.handle
        .push_tool_result(ConversationItem::tool_result("c1", "z".repeat(100_000)));

    let compacted = vec![ConversationItem::user("s".repeat(2_000))];
    h.handle.replace_conversation_for_compaction(compacted);

    let total = h.handle.get_total_tokens().await;
    assert_eq!(
        total, 5_500,
        "overhead ratio must be measured against the last-response estimate \
         (1k), not the live estimate inflated by the 25k post-response tool \
         result, got {total}"
    );
}

#[tokio::test]
async fn compaction_overhead_unaffected_by_pruning_after_last_response() {
    let h = TestHarness::new();

    // 12 turns of large tool results so default pruning (10-turn age) fires.
    let mut conv = Vec::new();
    for i in 0..12 {
        conv.push(ConversationItem::user(format!("q{i}")));
        conv.push(ConversationItem::tool_result(
            format!("call-{i}"),
            "x".repeat(200_000),
        ));
    }
    h.handle.replace_conversation(conv.clone());
    for _ in 0..12 {
        h.handle.increment_prompt_index();
    }

    let estimate_at_response = crate::estimate_conversation_tokens(&conv);
    let provider_total = estimate_at_response + estimate_at_response / 2;
    h.handle.record_token_usage(provider_total);

    // Triggers pruning: old tool results are hard-cleared in place.
    h.handle.push_user_message(ConversationItem::user("new q"));
    let pruned_estimate = crate::estimate_conversation_tokens(&h.handle.get_conversation().await);
    assert!(
        pruned_estimate + 20_000 < estimate_at_response,
        "test setup must actually prune ({pruned_estimate} vs {estimate_at_response})"
    );

    let compacted = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("z".repeat(8_000)),
    ];
    let compacted_estimate = crate::estimate_conversation_tokens(&compacted);
    h.handle.replace_conversation_for_compaction(compacted);

    let total = h.handle.get_total_tokens().await;
    let expected = (compacted_estimate as f64
        * (provider_total as f64 / estimate_at_response as f64))
        .round() as u64;
    assert_eq!(
        total, expected,
        "overhead ratio must come from the snapshot at the last response, \
         not be inflated by content pruned afterwards"
    );
}

#[tokio::test]
async fn restore_snapshot_preserves_frozen_estimate_for_overhead() {
    let h = TestHarness::new();
    h.handle
        .push_user_message(ConversationItem::user("q".repeat(4000)));
    let frozen = crate::estimate_conversation_tokens(&h.handle.get_conversation().await);
    let provider_total = frozen + 30_000;
    h.handle.record_token_usage(provider_total);

    // Rewind-style restore: trim the conversation, keep token fields.
    let mut snap = h.handle.snapshot().await.unwrap();
    assert_eq!(snap.estimate_at_last_response, frozen);
    snap.conversation.truncate(0);
    h.handle.restore_snapshot(snap);

    let compacted = vec![ConversationItem::user("z".repeat(400))];
    let compacted_estimate = crate::estimate_conversation_tokens(&compacted);
    h.handle.replace_conversation_for_compaction(compacted);

    let expected =
        (compacted_estimate as f64 * (provider_total as f64 / frozen as f64)).round() as u64;
    assert_eq!(
        h.handle.get_total_tokens().await,
        expected,
        "overhead ratio must use the frozen estimate carried through the snapshot"
    );
}

#[tokio::test]
async fn restore_snapshot_without_frozen_estimate_falls_back_to_recompute() {
    let h = TestHarness::new();
    h.handle
        .push_user_message(ConversationItem::user("q".repeat(4000)));
    let provider_total = 31_000;
    h.handle.record_token_usage(provider_total);

    // Pre-field snapshot (serde default): estimate_at_last_response == 0.
    let mut snap = h.handle.snapshot().await.unwrap();
    snap.estimate_at_last_response = 0;
    h.handle.restore_snapshot(snap);
    let recomputed = crate::estimate_conversation_tokens(&h.handle.get_conversation().await);

    let compacted = vec![ConversationItem::user("z".repeat(400))];
    let compacted_estimate = crate::estimate_conversation_tokens(&compacted);
    h.handle.replace_conversation_for_compaction(compacted);

    let total = h.handle.get_total_tokens().await;
    let expected =
        (compacted_estimate as f64 * (provider_total as f64 / recomputed as f64)).round() as u64;
    assert_eq!(
        total, expected,
        "legacy snapshots must fall back to re-estimating the restored conversation"
    );
}

#[tokio::test]
async fn compaction_reseed_without_provider_count_matches_plain_estimate() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("hello"));

    let compacted = vec![ConversationItem::user("w".repeat(8000))];
    h.handle.replace_conversation_for_compaction(compacted);

    let total = h.handle.get_total_tokens().await;
    assert_eq!(
        total, 2_000,
        "fresh-session reseed must be the raw estimate"
    );
}

#[tokio::test]
async fn non_compaction_replace_does_not_carry_overhead() {
    let h = TestHarness::new();
    h.handle
        .push_user_message(ConversationItem::user("x".repeat(4000)));
    h.handle.record_token_usage(51_000);

    h.handle
        .replace_conversation(vec![ConversationItem::user("q".repeat(4000))]);

    let total = h.handle.get_total_tokens().await;
    assert_eq!(
        total, 1_000,
        "non-compaction replace (e.g. rewind) keeps the plain estimate"
    );
}

#[tokio::test]
async fn flush_calls_persistence_flush() {
    let mut h = TestHarness::new();
    h.handle.flush();

    // Use a query as sync point to ensure flush was processed
    let _ = h.handle.get_prompt_index().await;

    let records = h.drain_persistence();
    assert_eq!(records.len(), 1);
    assert!(matches!(&records[0], PersistenceRecord::Flush));
}

#[tokio::test]
async fn restore_snapshot_restores_all_fields() {
    let mut h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("msg"));
    h.handle.record_token_usage(500);
    h.handle.increment_prompt_index();

    // Drain events from the mutations above
    let _ = h.handle.get_conversation().await;
    h.drain_events();

    let snapshot = h.handle.snapshot().await.unwrap();
    assert_eq!(snapshot.prompt_index, 1);
    assert_eq!(snapshot.total_tokens, 500);
    assert_eq!(snapshot.conversation.len(), 1);

    // Replace state
    h.handle.replace_conversation(vec![]);
    let _ = h.handle.get_conversation().await;

    // Restore
    h.handle.restore_snapshot(snapshot);

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 1);
    let idx = h.handle.get_prompt_index().await;
    assert_eq!(idx, 1);
    let tokens = h.handle.get_total_tokens().await;
    assert_eq!(tokens, 500);
}

// ============================================================================
// Query tests
// ============================================================================

#[tokio::test]
async fn get_conversation_returns_current_state() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("msg1"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("resp1"));

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 2);
}

#[tokio::test]
async fn get_total_tokens_returns_zero_initially() {
    let h = TestHarness::new();
    let tokens = h.handle.get_total_tokens().await;
    assert_eq!(tokens, 0);
}

#[tokio::test]
async fn replace_system_head_swaps_head_and_preserves_turns() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("old"),
        ConversationItem::user("hi"),
        ConversationItem::assistant("yo"),
    ]);
    let changed = h.handle.replace_system_head("new prompt").await;
    assert_eq!(changed, Some(true));
    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 3, "must not wipe user/assistant turns");
    assert!(matches!(&conv[0], ConversationItem::System(s) if s.content.as_ref() == "new prompt"));
    assert!(matches!(conv[1], ConversationItem::User(_)));
    assert!(matches!(conv[2], ConversationItem::Assistant(_)));
}

#[tokio::test]
async fn replace_system_head_noop_when_head_matches_modulo_newline() {
    let mut h = TestHarness::with_conversation(vec![
        ConversationItem::system("same\n"),
        ConversationItem::user("hi"),
    ]);
    let _ = h.drain_persistence(); // clear any seed writes
    let changed = h.handle.replace_system_head("same").await;
    assert_eq!(
        changed,
        Some(false),
        "trailing-newline-only diff is a no-op"
    );
    let conv = h.handle.get_conversation().await;
    assert!(matches!(&conv[0], ConversationItem::System(s) if s.content.as_ref() == "same\n"));
    assert!(
        h.drain_persistence().is_empty(),
        "a no-op must not re-persist"
    );
}

#[tokio::test]
async fn replace_system_head_inserts_when_absent() {
    let h = TestHarness::with_conversation(vec![ConversationItem::user("hi")]);
    let changed = h.handle.replace_system_head("sys").await;
    assert_eq!(changed, Some(true));
    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 2, "inserts System at head, keeps the user turn");
    assert!(matches!(&conv[0], ConversationItem::System(s) if s.content.as_ref() == "sys"));
    assert!(matches!(conv[1], ConversationItem::User(_)));
}

/// Lost-update safety: an item pushed just before the head swap survives, because
/// both operations serialize through the actor mailbox — the swap acts on the
/// actor's current conversation, never a stale caller-side snapshot. This is the
/// property that makes a mid-turn reconnect safe.
#[tokio::test]
async fn replace_system_head_retains_concurrently_pushed_item() {
    let h = TestHarness::with_conversation(vec![ConversationItem::system("old")]);
    h.handle
        .push_assistant_response(ConversationItem::assistant("in-flight turn output"));
    let changed = h.handle.replace_system_head("new").await;
    assert_eq!(changed, Some(true));
    let conv = h.handle.get_conversation().await;
    assert_eq!(
        conv.len(),
        2,
        "the item pushed before the swap must not be lost"
    );
    assert!(matches!(&conv[0], ConversationItem::System(s) if s.content.as_ref() == "new"));
    assert!(matches!(conv[1], ConversationItem::Assistant(_)));
}

/// A head swap during an active turn capture must not drop the in-flight
/// turn's captured tail (regression: `mem::take`ing the conversation before
/// `replace_conversation` snapshotted it emptied the tail — a `debug_assert`
/// panic in dev, a silently truncated capture in release).
#[tokio::test]
async fn replace_system_head_preserves_active_turn_capture() {
    let h = TestHarness::with_conversation(vec![ConversationItem::system("old")]);
    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("hello"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("in-flight"));

    let changed = h.handle.replace_system_head("new prompt").await;
    assert_eq!(changed, Some(true));

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture was active");
    assert_eq!(
        capture.messages.len(),
        2,
        "mid-turn head swap must not drop the captured turn tail"
    );
    assert!(matches!(&capture.messages[0], ConversationItem::User(_)));
    assert!(matches!(
        &capture.messages[1],
        ConversationItem::Assistant(_)
    ));
}

#[tokio::test]
async fn empty_conversation_queries_return_defaults() {
    let h = TestHarness::new();
    assert!(h.handle.get_conversation().await.is_empty());
    assert_eq!(h.handle.get_prompt_index().await, 0);
    assert_eq!(h.handle.get_total_tokens().await, 0);
    assert!(h.handle.get_agent_edited_paths().await.is_empty());
}

#[tokio::test]
async fn check_auto_compact_returns_none_when_under_threshold() {
    let h = TestHarness::with_context_window(10000);
    h.handle.record_token_usage(100);
    // Sync point
    let _ = h.handle.get_total_tokens().await;

    let trigger = h.handle.check_auto_compact_needed(85).await;
    assert!(trigger.is_none());
}

#[tokio::test]
async fn check_auto_compact_triggers_at_threshold() {
    let h = TestHarness::with_context_window(10000);
    h.handle.record_token_usage(8600);
    let _ = h.handle.get_total_tokens().await;

    let trigger = h.handle.check_auto_compact_needed(85).await;
    assert!(trigger.is_some());
    let t = trigger.unwrap();
    assert_eq!(t.total_tokens, 8600);
    assert_eq!(t.context_window.get(), 10000);
    assert_eq!(t.utilization_percent, 86);
}

// ============================================================================
// Edge-case / integration tests
// ============================================================================

#[tokio::test]
async fn record_agent_edited_path_deduplicates() {
    let h = TestHarness::new();
    h.handle.record_agent_edited_path("src/main.rs".to_string());
    h.handle.record_agent_edited_path("src/main.rs".to_string());
    h.handle.record_agent_edited_path("src/lib.rs".to_string());

    let paths = h.handle.get_agent_edited_paths().await;
    assert_eq!(paths.len(), 2);
}

#[tokio::test]
async fn cache_prompt_text_appends_in_order() {
    let h = TestHarness::new();
    h.handle.cache_prompt_text("first".to_string());
    h.handle.cache_prompt_text("second".to_string());
    h.handle.cache_prompt_text("third".to_string());

    let snap = h.handle.snapshot().await.unwrap();
    assert_eq!(snap.prompt_texts, vec!["first", "second", "third"]);
}

#[tokio::test]
async fn update_sampling_config_is_queryable() {
    let h = TestHarness::new();
    let new_config = SamplingConfig {
        base_url: "https://new.example.com".to_string(),
        model: "echo-agent-3".to_string(),
        max_completion_tokens: Some(4096),
        temperature: Some(0.5),
        top_p: None,
        api_backend: Default::default(),
        extra_headers: Default::default(),
        query_params: Default::default(),
        env_http_headers: Default::default(),
        context_window: NonZeroU64::new(200_000).unwrap(),
        reasoning_effort: None,
        stream_tool_calls: None,
        supports_tool_calls: None,
    };
    h.handle.update_sampling_config(new_config.clone());

    let config = h.handle.get_sampling_config().await.unwrap();
    assert_eq!(config.model, "echo-agent-3");
    assert_eq!(config.context_window, NonZeroU64::new(200_000).unwrap());
}

#[tokio::test]
async fn notification_meta_reflects_timing() {
    let h = TestHarness::new();
    h.handle.record_stream_start(1000);
    h.handle.record_turn_start(2000);

    let meta = h.handle.get_notification_meta().await.unwrap();
    assert_eq!(meta.stream_start_ms, Some(1000));
    assert_eq!(meta.turn_start_ms, Some(2000));
}

#[tokio::test]
async fn handle_clone_is_cheap_and_works() {
    let h = TestHarness::new();
    let handle2 = h.handle.clone();
    handle2.push_user_message(ConversationItem::user("from clone"));

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 1);
}

#[tokio::test]
async fn multiple_push_and_query_interleave() {
    let h = TestHarness::new();
    for i in 0..10 {
        h.handle
            .push_user_message(ConversationItem::user(format!("msg-{i}")));
    }
    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 10);
}

#[tokio::test]
async fn record_compaction_at_is_reflected_in_snapshot() {
    let h = TestHarness::new();
    h.handle.record_compaction_at(3);
    let snap = h.handle.snapshot().await.unwrap();
    assert_eq!(snap.last_compaction_prompt_index, Some(3));
}

#[tokio::test]
async fn truncate_removes_items_after_target_prompt_index() {
    let mut h = TestHarness::new();

    // Build 3 turns: system + 3x (user + assistant)
    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle.increment_prompt_index(); // 1
    h.handle.cache_prompt_text("q1".to_string());
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));

    h.handle.push_user_message(ConversationItem::user("q2"));
    h.handle.increment_prompt_index(); // 2
    h.handle.cache_prompt_text("q2".to_string());
    h.handle
        .push_assistant_response(ConversationItem::assistant("a2"));

    h.handle.push_user_message(ConversationItem::user("q3"));
    h.handle.increment_prompt_index(); // 3
    h.handle.cache_prompt_text("q3".to_string());
    h.handle
        .push_assistant_response(ConversationItem::assistant("a3"));

    // Sync + drain
    let _ = h.handle.get_prompt_index().await;
    h.drain_events();
    h.drain_persistence();

    // Truncate to prompt_index 1 → keep sys, q1, a1 (stop before q2)
    h.handle.truncate_to_prompt_index(1).await;

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 3); // sys + q1 + a1
    let idx = h.handle.get_prompt_index().await;
    assert_eq!(idx, 1);

    let snap = h.handle.snapshot().await.unwrap();
    assert_eq!(snap.prompt_texts, vec!["q1"]);

    // Verify persistence was called
    let records = h.drain_persistence();
    assert!(
        records
            .iter()
            .any(|r| matches!(r, PersistenceRecord::ReplaceHistory(_)))
    );
}

#[tokio::test]
async fn truncate_to_zero_keeps_only_system() {
    let mut h = TestHarness::new();

    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle.increment_prompt_index();
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));

    let _ = h.handle.get_prompt_index().await;
    h.drain_events();

    h.handle.truncate_to_prompt_index(0).await;

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 1); // just "sys"
    assert!(matches!(&conv[0], ConversationItem::System(_)));
    assert_eq!(h.handle.get_prompt_index().await, 0);
}

#[tokio::test]
async fn truncate_is_noop_when_already_at_target() {
    let mut h = TestHarness::new();
    h.handle.increment_prompt_index(); // 1

    let _ = h.handle.get_prompt_index().await;
    h.drain_events();
    h.drain_persistence();

    h.handle.truncate_to_prompt_index(1).await;
    h.handle.truncate_to_prompt_index(5).await;

    let idx = h.handle.get_prompt_index().await;
    assert_eq!(idx, 1);

    let events = h.drain_events();
    assert!(events.is_empty());
}

// ============================================================================
// Snapshot/restore comprehensive tests
// ============================================================================

#[tokio::test]
async fn snapshot_restore_preserves_all_fields() {
    let h = TestHarness::new();

    // Set up state with non-default values for EVERY field
    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));
    h.handle.record_token_usage(999);
    h.handle.increment_prompt_index();
    h.handle.record_agent_edited_path("src/foo.rs".to_string());
    h.handle.record_agent_edited_path("src/bar.rs".to_string());
    h.handle.cache_prompt_text("query 1".to_string());
    h.handle.record_stream_start(12345);
    h.handle.record_turn_start(12340);
    h.handle.record_compaction_at(0);

    let snapshot = h.handle.snapshot().await.unwrap();

    // Mutate everything to different values
    h.handle.replace_conversation(vec![]);
    h.handle.record_token_usage(0);
    h.handle.increment_prompt_index();
    h.handle.record_stream_start(99999);
    h.handle.record_turn_start(99990);
    h.handle.record_compaction_at(99);

    // Verify mutated
    let _ = h.handle.get_prompt_index().await;
    assert!(h.handle.get_conversation().await.is_empty());

    // Restore from snapshot
    h.handle.restore_snapshot(snapshot.clone());

    // Verify ALL fields match
    assert_eq!(
        h.handle.get_conversation().await.len(),
        snapshot.conversation.len()
    );
    assert_eq!(h.handle.get_total_tokens().await, snapshot.total_tokens);
    assert_eq!(h.handle.get_prompt_index().await, snapshot.prompt_index);
    assert_eq!(
        h.handle.get_agent_edited_paths().await,
        snapshot.agent_edited_paths
    );

    let meta = h.handle.get_notification_meta().await.unwrap();
    assert_eq!(meta.stream_start_ms, snapshot.stream_start_ms);
    assert_eq!(meta.turn_start_ms, snapshot.turn_start_ms);

    let restored_snap = h.handle.snapshot().await.unwrap();
    assert_eq!(restored_snap.prompt_texts, snapshot.prompt_texts);
    assert_eq!(
        restored_snap.last_compaction_prompt_index,
        snapshot.last_compaction_prompt_index
    );
}

#[tokio::test]
async fn auto_compact_does_not_trigger_below_threshold() {
    let h = TestHarness::with_context_window(10000);
    h.handle.record_token_usage(8400);
    let _ = h.handle.get_total_tokens().await;

    let trigger = h.handle.check_auto_compact_needed(85).await;
    assert!(trigger.is_none());
}

#[tokio::test]
async fn with_initial_conversation_preserves_items() {
    let items = vec![
        ConversationItem::system("sys prompt"),
        ConversationItem::user("initial msg"),
    ];
    let h = TestHarness::with_conversation(items);
    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 2);
}

// ============================================================================
// BuildConversationRequest tests
// ============================================================================

#[tokio::test]
async fn build_request_includes_all_messages() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle.push_user_message(ConversationItem::user("hello"));
    // Sync point
    let _ = h.handle.get_conversation().await;

    let request = h
        .handle
        .build_request(vec![], None, false, None, "conv-1".into(), "req-1".into())
        .await
        .unwrap();
    assert_eq!(request.items.len(), 2);
    assert_eq!(request.x_echo_agent_conv_id, Some("conv-1".to_string()));
    assert_eq!(request.x_echo_agent_req_id, Some("req-1".to_string()));
}

#[tokio::test]
async fn build_request_projects_agent_message_for_model_without_mutating_history() {
    let raw = format!(
        "{}\npayload starts with the exact label",
        crate::compaction_utils::AGENT_MESSAGE_MODEL_LABEL
    );
    let raw_item = ConversationItem::agent_message(&raw);
    let raw_bytes = serde_json::to_vec(&raw_item).unwrap();
    let h = TestHarness::with_conversation(vec![raw_item]);

    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();
    assert_eq!(
        request.items[0].text_content(),
        format!(
            "{}\n{raw}",
            crate::compaction_utils::AGENT_MESSAGE_MODEL_LABEL
        )
    );

    let persisted = h.handle.get_conversation().await;
    assert_eq!(persisted[0].text_content(), raw);
    assert_eq!(serde_json::to_vec(&persisted[0]).unwrap(), raw_bytes);
}

#[tokio::test]
async fn build_request_with_empty_conversation() {
    let h = TestHarness::new();
    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();
    assert!(request.items.is_empty());
}

#[tokio::test]
async fn build_request_preserves_system_message() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("You are a coding assistant."),
        ConversationItem::user("hi"),
    ]);
    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();
    assert_eq!(request.items.len(), 2);
    if let ConversationItem::System(ref sys) = request.items[0] {
        assert_eq!(sys.content.as_ref(), "You are a coding assistant.");
    } else {
        panic!("expected System item");
    }
}

#[tokio::test]
async fn build_request_injects_memory_reminder() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("You are helpful."),
        ConversationItem::user("hi"),
    ]);
    let request = h
        .handle
        .build_request(
            vec![],
            Some("Remember: user prefers Rust".to_string()),
            false,
            None,
            "c".into(),
            "r".into(),
        )
        .await
        .unwrap();

    if let ConversationItem::System(ref sys) = request.items[0] {
        assert!(sys.content.contains("Remember: user prefers Rust"));
        assert!(sys.content.starts_with("You are helpful."));
    } else {
        panic!("expected System item");
    }
}

#[tokio::test]
async fn build_request_injects_memory_when_no_system() {
    let h = TestHarness::with_conversation(vec![ConversationItem::user("hi")]);
    let request = h
        .handle
        .build_request(
            vec![],
            Some("Remember this".to_string()),
            false,
            None,
            "c".into(),
            "r".into(),
        )
        .await
        .unwrap();

    assert_eq!(request.items.len(), 2); // new System + original User
    assert!(matches!(&request.items[0], ConversationItem::System(_)));
}

#[tokio::test]
async fn build_request_repairs_dangling_tool_calls() {
    use echo_agent_sampling_types::ToolCall;

    // Repair happens in ChatState::new() (called by with_conversation → spawn),
    // not in build_request. Both handler and clone-level repair are no-ops here.
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("do something"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "my_tool".to_string(),
            arguments: "{}".into(),
        }]),
        // No ToolResult for call_1 — repaired by ChatState::new() before any command.
    ]);

    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();

    // Synthetic ToolResult present (inserted at construction, not at request time).
    assert_eq!(request.items.len(), 4);
    assert!(matches!(&request.items[3], ConversationItem::ToolResult(_)));
}

#[tokio::test]
async fn build_request_with_tool_definitions() {
    use echo_agent_sampling_types::ToolSpec;

    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hello"),
    ]);

    let tools = vec![ToolSpec {
        name: "read_file".to_string(),
        description: Some("Read a file".to_string()),
        parameters: serde_json::json!({"type": "object"}),
    }];

    let request = h
        .handle
        .build_request(tools, None, false, None, "c".into(), "r".into())
        .await
        .unwrap();

    assert_eq!(request.tools.len(), 1);
    assert_eq!(request.tools[0].name, "read_file");
}

#[tokio::test]
async fn build_request_uses_sampling_config() {
    let config = SamplingConfig {
        base_url: "https://api.example.com".to_string(),
        model: "echo-agent-3".to_string(),
        max_completion_tokens: Some(8192),
        temperature: Some(0.7),
        top_p: Some(0.9),
        api_backend: Default::default(),
        extra_headers: Default::default(),
        query_params: Default::default(),
        env_http_headers: Default::default(),
        context_window: NonZeroU64::new(128_000).unwrap(),
        reasoning_effort: None,
        stream_tool_calls: None,
        supports_tool_calls: None,
    };
    let h = TestHarness::with_config(vec![ConversationItem::user("hi")], config);

    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();

    assert_eq!(request.model, Some("echo-agent-3".to_string()));
    assert_eq!(request.temperature, Some(0.7));
    assert_eq!(request.max_output_tokens, Some(8192));
    assert_eq!(request.top_p, Some(0.9));
}

#[tokio::test]
async fn build_request_does_not_mutate_actor_state() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hi"),
    ]);

    // Build request (which clones + potentially mutates the clone)
    let _ = h
        .handle
        .build_request(
            vec![],
            Some("injected memory".to_string()),
            false,
            None,
            "c".into(),
            "r".into(),
        )
        .await
        .unwrap();

    // Actor's own conversation should be unchanged
    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 2);
    if let ConversationItem::System(ref sys) = conv[0] {
        assert_eq!(sys.content.as_ref(), "sys"); // no memory injected into original
    }
}

#[tokio::test]
async fn build_request_can_persist_memory_into_actor_state() {
    let mut h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hi"),
    ]);

    let request = h
        .handle
        .build_request(
            vec![],
            Some("<memory-context>\nRemember this\n</memory-context>".to_string()),
            true,
            None,
            "c".into(),
            "r".into(),
        )
        .await
        .unwrap();

    if let ConversationItem::System(ref sys) = request.items[0] {
        assert!(sys.content.contains("Remember this"));
    } else {
        panic!("expected System item in request");
    }

    let conv = h.handle.get_conversation().await;
    if let ConversationItem::System(ref sys) = conv[0] {
        assert!(sys.content.contains("Remember this"));
    } else {
        panic!("expected persisted System item");
    }

    let records = h.drain_persistence();
    assert!(
        records
            .iter()
            .any(|r| matches!(r, PersistenceRecord::ReplaceHistory(items) if matches!(items.first(), Some(ConversationItem::System(sys)) if sys.content.contains("Remember this"))))
    );
}

#[tokio::test]
async fn build_request_with_multiple_tool_calls_and_results() {
    use echo_agent_sampling_types::ToolCall;

    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("do things"),
        ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "call_1".into(),
                name: "tool_a".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "tool_b".to_string(),
                arguments: "{}".into(),
            },
        ]),
        ConversationItem::tool_result("call_1", "result a"),
        ConversationItem::tool_result("call_2", "result b"),
        ConversationItem::assistant("Done!"),
    ]);

    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();

    // All items should pass through (no dangling calls, no pruning needed)
    assert_eq!(request.items.len(), 6);
}

// ============================================================================
// Parallel tool calls with mixed accept/reject
// ============================================================================

/// Simulates the exact sequence that `echo-agent-runtime`'s `execute_tool_calls`
/// produces when the model emits 3 parallel tool calls and:
///   - Tool #1 (read_file):       user **accepts** → executed successfully
///   - Tool #2 (edit_file):       user **rejects** → handle_tool_not_executed
///   - Tool #3 (run_terminal_cmd): **skipped** due to earlier rejection
///
/// In the shell, `execute_tool_calls` iterates sequentially. When tool #2 is
/// rejected, `final_result` is set to `PermissionReject`, causing tool #3 to
/// be skipped with a synthetic cancellation message pushed as a ToolResult.
///
/// The conversation should end up as:
///   [0] System
///   [1] User
///   [2] Assistant (3 tool calls)
///   [3] ToolResult for call_1 (success)
///   [4] ToolResult for call_2 (rejection reason)
///   [5] ToolResult for call_3 (cancellation due to earlier rejection)
#[tokio::test]
async fn parallel_tool_calls_accept_first_reject_second_skip_third() {
    use echo_agent_sampling_types::ToolCall;

    let h = TestHarness::new();

    // ── Turn setup ──────────────────────────────────────────────────────
    // System prompt
    h.handle.push_user_message(ConversationItem::system(
        "You are a helpful coding assistant.",
    ));

    // User asks the model to do something complex
    h.handle.push_user_message(ConversationItem::user(
        "Read main.rs, fix the typo, then run the tests",
    ));

    h.handle.increment_prompt_index();

    // ── Model response: 3 parallel tool calls ───────────────────────────
    // The model's single assistant message contains all 3 tool calls.
    // In the real code, this is built from the streaming response and pushed
    // via `push_assistant_response`.
    let assistant_with_tools =
        ConversationItem::Assistant(echo_agent_sampling_types::AssistantItem {
            content: "I'll read the file, fix it, and run tests.".into(),
            tool_calls: vec![
                ToolCall {
                    id: "call_1".into(),
                    name: "read_file".to_string(),
                    arguments: r#"{"target_file":"src/main.rs"}"#.into(),
                },
                ToolCall {
                    id: "call_2".into(),
                    name: "edit_file".to_string(),
                    arguments: r#"{"target_file":"src/main.rs","new_string":"fixed"}"#.into(),
                },
                ToolCall {
                    id: "call_3".into(),
                    name: "run_terminal_cmd".to_string(),
                    arguments: r#"{"command":"cargo test"}"#.into(),
                },
            ],
            model_id: Some("echo-agent-3".to_string()),
            model_fingerprint: None,
            reasoning_effort: None,
        });
    h.handle.push_assistant_response(assistant_with_tools);

    // ── Tool execution results (simulating execute_tool_calls) ──────────

    // Tool #1: read_file — user accepted, tool executed successfully
    h.handle.push_tool_result(ConversationItem::tool_result(
        "call_1",
        "fn main() {\n    println!(\"hello wrold\");\n}",
    ));

    // Tool #2: edit_file — user rejected via permission prompt
    // In the shell, `handle_tool_not_executed` pushes a ToolResult with the
    // rejection reason and returns ToolLoop::PermissionReject.
    h.handle.push_tool_result(ConversationItem::tool_result(
        "call_2",
        "User rejected: permission denied for tool `edit_file`",
    ));

    // Tool #3: run_terminal_cmd — skipped because tool #2 was rejected.
    // In `execute_tool_calls`, when `final_result` is set, remaining tools
    // get a synthetic cancellation message.
    h.handle.push_tool_result(ConversationItem::tool_result(
        "call_3",
        "Tool execution cancelled due to earlier permission rejection for tool `run_terminal_cmd`",
    ));

    // ── Verify the conversation state ───────────────────────────────────
    let conv = h.handle.get_conversation().await;

    // Expected: System + User + Assistant(3 calls) + 3 ToolResults = 6 items
    assert_eq!(
        conv.len(),
        6,
        "expected 6 conversation items, got {}",
        conv.len()
    );

    // [0] System
    assert!(
        matches!(&conv[0], ConversationItem::System(s) if s.content.as_ref() == "You are a helpful coding assistant."),
        "item[0] should be the system prompt"
    );

    // [1] User
    assert!(
        matches!(&conv[1], ConversationItem::User(_)),
        "item[1] should be the user message"
    );

    // [2] Assistant with 3 tool calls
    match &conv[2] {
        ConversationItem::Assistant(a) => {
            assert_eq!(a.tool_calls.len(), 3, "assistant should have 3 tool calls");
            assert_eq!(a.tool_calls[0].id.as_ref(), "call_1");
            assert_eq!(a.tool_calls[0].name, "read_file");
            assert_eq!(a.tool_calls[1].id.as_ref(), "call_2");
            assert_eq!(a.tool_calls[1].name, "edit_file");
            assert_eq!(a.tool_calls[2].id.as_ref(), "call_3");
            assert_eq!(a.tool_calls[2].name, "run_terminal_cmd");
            assert_eq!(
                a.content.as_ref(),
                "I'll read the file, fix it, and run tests."
            );
        }
        other => panic!("item[2] should be Assistant, got {:?}", other),
    }

    // [3] ToolResult for call_1 — success
    match &conv[3] {
        ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "call_1");
            assert!(
                tr.content.contains("hello wrold"),
                "tool_result for call_1 should contain the file content"
            );
        }
        other => panic!("item[3] should be ToolResult, got {:?}", other),
    }

    // [4] ToolResult for call_2 — rejected
    match &conv[4] {
        ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "call_2");
            assert!(
                tr.content.contains("rejected") || tr.content.contains("denied"),
                "tool_result for call_2 should indicate rejection, got: {}",
                tr.content
            );
        }
        other => panic!("item[4] should be ToolResult, got {:?}", other),
    }

    // [5] ToolResult for call_3 — cancelled due to earlier rejection
    match &conv[5] {
        ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "call_3");
            assert!(
                tr.content.contains("cancelled")
                    && tr.content.contains("earlier permission rejection"),
                "tool_result for call_3 should indicate cancellation due to earlier rejection, got: {}",
                tr.content
            );
        }
        other => panic!("item[5] should be ToolResult, got {:?}", other),
    }
}

/// After parallel tool calls with rejection, verify that `build_request`
/// sees no dangling tool calls (every call has a matching ToolResult).
/// This is important because dangling calls trigger synthetic repair which
/// would corrupt the rejection messages.
#[tokio::test]
async fn parallel_tool_calls_with_rejection_has_no_dangling_calls() {
    use echo_agent_sampling_types::ToolCall;

    let h = TestHarness::new();

    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle
        .push_user_message(ConversationItem::user("do things"));

    // Assistant with 3 parallel tool calls
    h.handle
        .push_assistant_response(ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "edit_file".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_3".into(),
                name: "run_terminal_cmd".to_string(),
                arguments: "{}".into(),
            },
        ]));

    // All 3 get ToolResults (accept, reject, skip)
    h.handle
        .push_tool_result(ConversationItem::tool_result("call_1", "file contents"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call_2", "rejected by user"));
    h.handle.push_tool_result(ConversationItem::tool_result(
        "call_3",
        "Tool execution cancelled due to earlier permission rejection for tool `run_terminal_cmd`",
    ));

    // Build request — should NOT add any synthetic ToolResults
    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();

    // 2 (sys+user) + 1 (assistant) + 3 (tool results) = 6
    assert_eq!(
        request.items.len(),
        6,
        "build_request should not insert synthetic tool results when all calls have results"
    );

    // Verify the tool results are preserved with their original content
    let tool_results: Vec<_> = request
        .items
        .iter()
        .filter_map(|item| {
            if let ConversationItem::ToolResult(tr) = item {
                Some(tr)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(tool_results.len(), 3);
    assert_eq!(tool_results[0].tool_call_id, "call_1");
    assert_eq!(tool_results[0].content.as_ref(), "file contents");
    assert_eq!(tool_results[1].tool_call_id, "call_2");
    assert_eq!(tool_results[1].content.as_ref(), "rejected by user");
    assert_eq!(tool_results[2].tool_call_id, "call_3");
    assert!(tool_results[2].content.contains("cancelled"));
}

/// Verify that persistence records all 5 pushes (assistant + 3 tool results)
/// correctly for the parallel tool call scenario.
#[tokio::test]
async fn parallel_tool_calls_with_rejection_persists_all_items() {
    use echo_agent_sampling_types::ToolCall;

    let mut h = TestHarness::new();

    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle
        .push_user_message(ConversationItem::user("do things"));
    h.handle
        .push_assistant_response(ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "edit_file".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_3".into(),
                name: "run_terminal_cmd".to_string(),
                arguments: "{}".into(),
            },
        ]));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call_1", "success"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call_2", "rejected"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call_3", "cancelled"));

    // Sync point
    let _ = h.handle.get_conversation().await;

    // All 6 items should have been persisted as Message records
    let records = h.drain_persistence();
    let message_count = records
        .iter()
        .filter(|r| matches!(r, PersistenceRecord::Message(_)))
        .count();

    assert_eq!(
        message_count, 6,
        "expected 6 persisted messages (sys + user + assistant + 3 tool results), got {}",
        message_count
    );
}

// ============================================================================
// Race condition: cancellation mid-tool-execution → dangling calls on reload
// ============================================================================

/// Simulates the race condition where:
///   1. Model emits 3 parallel tool calls (single assistant message)
///   2. Tool #1 executes and its result is persisted
///   3. User cancels (Ctrl+C) or app crashes BEFORE tool #2/#3 results are pushed
///   4. On session reload, chat_history.jsonl has the assistant (3 calls) + only 1 result
///
/// `ChatState::new` now repairs dangling tool calls eagerly at initialization,
/// so the actor's in-memory conversation is clean from the start — not just the
/// clone produced by `build_request`.
#[tokio::test]
async fn dangling_tool_calls_after_crash_are_repaired_on_load() {
    use echo_agent_sampling_types::ToolCall;

    // Simulate what chat_history.jsonl looks like after a crash:
    // The assistant message (with 3 tool calls) was persisted, and only
    // tool #1's result was persisted before the process died.
    let crashed_conversation = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Read, edit, and test"),
        ConversationItem::Assistant(echo_agent_sampling_types::AssistantItem {
            content: std::sync::Arc::<str>::from(""),
            tool_calls: vec![
                ToolCall {
                    id: "call_1".into(),
                    name: "read_file".to_string(),
                    arguments: r#"{"target_file":"src/main.rs"}"#.into(),
                },
                ToolCall {
                    id: "call_2".into(),
                    name: "edit_file".to_string(),
                    arguments: r#"{"target_file":"src/main.rs","new_string":"fixed"}"#.into(),
                },
                ToolCall {
                    id: "call_3".into(),
                    name: "run_terminal_cmd".to_string(),
                    arguments: r#"{"command":"cargo test"}"#.into(),
                },
            ],
            model_id: Some("echo-agent-3".to_string()),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        // Only call_1 got persisted before the crash
        ConversationItem::tool_result("call_1", "fn main() { ... }"),
        // call_2 and call_3 are MISSING — this is the dangling state
    ];

    // "Reload" the session by creating an actor with the crashed conversation.
    // ChatState::new repairs dangling tool calls eagerly.
    let h = TestHarness::with_conversation(crashed_conversation);

    // The actor's conversation should already be repaired (6 items, not 4)
    let conv = h.handle.get_conversation().await;
    assert_eq!(
        conv.len(),
        6,
        "actor should repair dangling calls on load: expected 6 items (sys + user + assistant + 3 results), got {}",
        conv.len()
    );

    // Verify the tool results
    let tool_results: Vec<_> = conv
        .iter()
        .filter_map(|item| {
            if let ConversationItem::ToolResult(tr) = item {
                Some(tr)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        tool_results.len(),
        3,
        "should have 3 tool results (1 real + 2 synthetic)"
    );

    // call_1: real result (persisted before crash)
    assert_eq!(tool_results[0].tool_call_id, "call_1");
    assert!(
        tool_results[0].content.contains("fn main"),
        "call_1 should have the original result"
    );

    // call_2: synthetic repair
    assert_eq!(tool_results[1].tool_call_id, "call_2");
    assert!(
        tool_results[1].content.contains("cancelled")
            || tool_results[1].content.contains("not executed"),
        "call_2 should have a synthetic cancellation result, got: {}",
        tool_results[1].content
    );

    // call_3: synthetic repair
    assert_eq!(tool_results[2].tool_call_id, "call_3");
    assert!(
        tool_results[2].content.contains("cancelled")
            || tool_results[2].content.contains("not executed"),
        "call_3 should have a synthetic cancellation result, got: {}",
        tool_results[2].content
    );

    // build_request should also see 6 items (no double-repair)
    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();
    assert_eq!(
        request.items.len(),
        6,
        "build_request should not double-repair already-fixed conversation"
    );
}

/// Verify that both the actor state AND the build_request output are
/// consistent after repair — the synthetic results appear in both.
#[tokio::test]
async fn dangling_tool_calls_repair_is_consistent_between_state_and_request() {
    use echo_agent_sampling_types::ToolCall;

    let crashed_conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("do stuff"),
        ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "edit_file".to_string(),
                arguments: "{}".into(),
            },
        ]),
        // Only call_1 has a result — call_2 is dangling
        ConversationItem::tool_result("call_1", "ok"),
    ];

    let h = TestHarness::with_conversation(crashed_conversation);

    // Actor state should have 5 items (repaired on load)
    let conv = h.handle.get_conversation().await;
    assert_eq!(
        conv.len(),
        5,
        "actor state should be repaired on load: sys + user + assistant + 2 results"
    );

    // build_request should match (no extra synthetic results)
    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();
    assert_eq!(
        request.items.len(),
        5,
        "build_request should match actor state length"
    );
}

/// Worst case: crash happens right after the assistant message is persisted
/// but BEFORE any tool results. All 3 tool calls are dangling.
/// ChatState::new should repair all 3 eagerly.
#[tokio::test]
async fn all_tool_calls_dangling_after_crash() {
    use echo_agent_sampling_types::ToolCall;

    let crashed_conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("do everything"),
        ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "edit_file".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_3".into(),
                name: "run_terminal_cmd".to_string(),
                arguments: "{}".into(),
            },
        ]),
        // NO tool results at all — complete crash right after assistant was persisted
    ];

    let h = TestHarness::with_conversation(crashed_conversation);

    // Actor state should be repaired: sys + user + assistant + 3 synthetic results = 6
    let conv = h.handle.get_conversation().await;
    assert_eq!(
        conv.len(),
        6,
        "all 3 dangling calls should be repaired on load"
    );

    let tool_results: Vec<_> = conv
        .iter()
        .filter_map(|item| {
            if let ConversationItem::ToolResult(tr) = item {
                Some(tr)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(tool_results.len(), 3);

    // All should be synthetic cancellation messages
    for (i, tr) in tool_results.iter().enumerate() {
        assert_eq!(tr.tool_call_id, format!("call_{}", i + 1));
        assert!(
            tr.content.contains("cancelled") || tr.content.contains("not executed"),
            "call_{} should have synthetic cancellation, got: {}",
            i + 1,
            tr.content
        );
    }

    // build_request should also see 6 items — no double-repair
    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();
    assert_eq!(request.items.len(), 6);
}

// ============================================================================
// Live-session cancellation: user cancels mid-tool-execution (no restart)
// ============================================================================

/// Simulates an in-session abort where:
///   1. Model emits 3 parallel tool calls → assistant pushed to conversation
///   2. User immediately cancels (Ctrl+C) → tokio task aborted
///   3. Zero tool results pushed (abort happened before execute_tool_calls)
///   4. TUI stays alive, user types a new prompt
///
/// This is different from the reload scenario: `ChatState::new` doesn't run
/// again because the actor is still alive. The fix is that `push_user_message`
/// now calls `repair_dangling_tool_calls` before appending the new user
/// message, so the conversation is cleaned up in-place.
#[tokio::test]
async fn live_cancel_before_any_tool_execution_repairs_on_next_user_message() {
    use echo_agent_sampling_types::ToolCall;

    let h = TestHarness::new();

    // ── Turn 1: normal conversation ─────────────────────────────────────
    h.handle
        .push_user_message(ConversationItem::system("You are a helpful assistant."));
    h.handle.push_user_message(ConversationItem::user("Hello"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("Hi! How can I help?"));

    // ── Turn 2: model wants 3 tool calls, user cancels immediately ──────
    h.handle
        .push_user_message(ConversationItem::user("Read, edit, and test everything"));

    // Model streams its response → assistant with 3 tool calls is pushed
    h.handle
        .push_assistant_response(ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: r#"{"target_file":"src/main.rs"}"#.into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "edit_file".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_3".into(),
                name: "run_terminal_cmd".to_string(),
                arguments: r#"{"command":"cargo test"}"#.into(),
            },
        ]));

    // *** USER CANCELS HERE (Ctrl+C) ***
    // The tokio task is aborted. execute_tool_calls never ran.
    // Zero ToolResult items pushed. The conversation has dangling calls.

    // get_conversation() and snapshot() are pure reads — they do NOT repair.
    // The dangling calls are visible in the raw state until the next write boundary.
    let conv_before = h.handle.get_conversation().await;
    // sys + user("Hello") + assistant("Hi!") + user("Read...") + assistant(3 calls) = 5
    assert_eq!(
        conv_before.len(),
        5,
        "get_conversation() should be a pure read (no repair), got {} items",
        conv_before.len()
    );

    let snap = h.handle.snapshot().await.unwrap();
    assert_eq!(
        snap.conversation.len(),
        5,
        "snapshot() should be a pure read (no repair)"
    );

    // ── Turn 3: push_user_message() is the write boundary; repairs here.
    h.handle
        .push_user_message(ConversationItem::user("Actually, just read the file"));

    let conv = h.handle.get_conversation().await;

    // sys + user + assistant + user + assistant(3 calls) + 3 repairs + user = 9
    assert_eq!(conv.len(), 9);

    // Verify the synthetic repairs are in the right place
    for (idx, expected_call_id) in [(5, "call_1"), (6, "call_2"), (7, "call_3")] {
        match &conv[idx] {
            ConversationItem::ToolResult(tr) => {
                assert_eq!(tr.tool_call_id, expected_call_id);
                assert!(
                    tr.content.contains("cancelled") || tr.content.contains("not executed"),
                    "item[{}] should be synthetic cancellation for {}, got: {}",
                    idx,
                    expected_call_id,
                    tr.content
                );
            }
            other => panic!(
                "item[{}] should be synthetic ToolResult for {}, got {:?}",
                idx, expected_call_id, other
            ),
        }
    }

    // New user message is at the end
    assert!(
        matches!(&conv[8], ConversationItem::User(_)),
        "item[8] should be the new user message"
    );

    // build_request should work cleanly — no dangling calls, no double-repair
    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();
    assert_eq!(request.items.len(), 9);
}

/// Partial cancellation: tool #1 result was pushed, then user cancelled.
/// Tools #2 and #3 are dangling. Next user message should repair only those.
#[tokio::test]
async fn live_cancel_after_partial_tool_results_repairs_remaining() {
    use echo_agent_sampling_types::ToolCall;

    let h = TestHarness::new();

    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle
        .push_user_message(ConversationItem::user("do everything"));

    // Model returns 3 parallel tool calls
    h.handle
        .push_assistant_response(ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "edit_file".to_string(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "call_3".into(),
                name: "run_terminal_cmd".to_string(),
                arguments: "{}".into(),
            },
        ]));

    // Tool #1 executed and result was pushed before abort
    h.handle.push_tool_result(ConversationItem::tool_result(
        "call_1",
        "file contents here",
    ));

    // *** USER CANCELS HERE — tool #2 and #3 never executed ***

    // User types a new prompt
    h.handle.push_user_message(ConversationItem::user(
        "never mind, just help me with something else",
    ));

    let conv = h.handle.get_conversation().await;

    // sys + user + assistant(3 calls) + result(call_1) + repair(call_2) + repair(call_3) + user(new) = 7
    assert_eq!(
        conv.len(),
        7,
        "expected 7 items after partial repair, got {}",
        conv.len()
    );

    // call_1 should still have the real result
    match &conv[3] {
        ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "call_1");
            assert_eq!(tr.content.as_ref(), "file contents here");
        }
        other => panic!(
            "item[3] should be real ToolResult for call_1, got {:?}",
            other
        ),
    }

    // call_2 and call_3 should be synthetic repairs
    for (idx, expected_call_id) in [(4, "call_2"), (5, "call_3")] {
        match &conv[idx] {
            ConversationItem::ToolResult(tr) => {
                assert_eq!(tr.tool_call_id, expected_call_id);
                assert!(
                    tr.content.contains("cancelled") || tr.content.contains("not executed"),
                    "item[{}] should be synthetic for {}, got: {}",
                    idx,
                    expected_call_id,
                    tr.content
                );
            }
            other => panic!("item[{}] should be ToolResult, got {:?}", idx, other),
        }
    }

    // New user message at the end
    assert!(matches!(&conv[6], ConversationItem::User(_)));
}

// Turn message capture tests
// ============================================================================

#[tokio::test]
async fn turn_capture_collects_all_message_types() {
    let h = TestHarness::new();

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("hello"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("hi"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-1", "result"));

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture was active");

    assert_eq!(capture.messages.len(), 3);
    assert!(matches!(&capture.messages[0], ConversationItem::User(_)));
    assert!(matches!(
        &capture.messages[1],
        ConversationItem::Assistant(_)
    ));
    assert!(matches!(
        &capture.messages[2],
        ConversationItem::ToolResult(_)
    ));
    assert!(!capture.compaction_occurred);
}

#[tokio::test]
async fn harness_trace_items_ride_own_turn_not_the_live_capture() {
    use echo_agent_sampling_types::ToolCall;
    let h = TestHarness::new();

    let live_before = h.handle.get_conversation().await.len();

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("u1"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));
    // Verifier-style: synthetic `task` pair recorded mid-turn must NOT splice
    // into the main turn capture — it rides its own harness trace turn.
    h.handle.append_harness_trace_items(vec![
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "verifier-1".into(),
            name: "spawn_subagent".into(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("verifier-1", "Refuted"),
    ]);
    h.handle.flush_harness_trace_turn();
    h.handle.push_user_message(ConversationItem::user("u2"));

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture was active");

    // The main turn capture holds only the live items — no harness pair.
    assert_eq!(capture.messages.len(), 3);
    assert!(matches!(&capture.messages[0], ConversationItem::User(_)));
    assert!(matches!(
        &capture.messages[1],
        ConversationItem::Assistant(_)
    ));
    assert!(matches!(&capture.messages[2], ConversationItem::User(_)));

    // The harness pair is drained as its own standalone trace turn.
    let harness = h.handle.take_harness_trace_turns().await;
    assert_eq!(harness.len(), 1, "one sealed verifier trace turn");
    assert_eq!(harness[0].len(), 2);
    assert!(matches!(
        &harness[0][0],
        ConversationItem::Assistant(a) if !a.tool_calls.is_empty()
    ));
    assert!(matches!(&harness[0][1], ConversationItem::ToolResult(_)));

    // Harness items never enter the live conversation fed to the model.
    let live_after = h.handle.get_conversation().await.len();
    assert_eq!(
        live_after - live_before,
        3,
        "live conversation gained only u1, a1, u2 — not the harness pair",
    );
}

#[tokio::test]
async fn harness_trace_recorded_before_capture_seals_into_own_turn() {
    use echo_agent_sampling_types::ToolCall;
    let h = TestHarness::new();

    // Planner-style: synthetic pair recorded BEFORE the turn's capture starts
    // (the goal planner runs during setup_goal, ahead of begin_turn_capture).
    h.handle.append_harness_trace_items(vec![
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "planner-1".into(),
            name: "spawn_subagent".into(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("planner-1", "Done"),
    ]);
    h.handle.flush_harness_trace_turn();

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("u1"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture was active");

    // The main capture holds only the live turn items — the planner pair does
    // not lead it.
    assert_eq!(capture.messages.len(), 2);
    assert!(matches!(&capture.messages[0], ConversationItem::User(_)));
    assert!(matches!(
        &capture.messages[1],
        ConversationItem::Assistant(_)
    ));

    // The planner pair is its own harness trace turn.
    let harness = h.handle.take_harness_trace_turns().await;
    assert_eq!(harness.len(), 1);
    assert_eq!(harness[0].len(), 2);
    assert!(matches!(
        &harness[0][0],
        ConversationItem::Assistant(a) if !a.tool_calls.is_empty()
    ));
    assert!(matches!(&harness[0][1], ConversationItem::ToolResult(_)));
}

#[tokio::test]
async fn harness_trace_turns_separate_per_flush_and_drain_clears() {
    use echo_agent_sampling_types::ToolCall;
    let h = TestHarness::new();

    let pair = |id: &str| {
        vec![
            ConversationItem::assistant_tool_calls(vec![ToolCall {
                id: id.into(),
                name: "spawn_subagent".into(),
                arguments: "{}".into(),
            }]),
            ConversationItem::tool_result(id, "Refuted"),
        ]
    };

    // Two verifier panels (rounds) each seal into their own turn; an
    // un-flushed third accumulator is sealed defensively on take.
    h.handle.append_harness_trace_items(pair("round-1"));
    h.handle.flush_harness_trace_turn();
    h.handle.append_harness_trace_items(pair("round-2"));
    h.handle.flush_harness_trace_turn();
    h.handle
        .append_harness_trace_items(pair("round-3-unflushed"));

    let harness = h.handle.take_harness_trace_turns().await;
    assert_eq!(harness.len(), 3, "two flushed + one defensively sealed");

    // Drain clears the buffer — a second take returns nothing.
    let second = h.handle.take_harness_trace_turns().await;
    assert!(second.is_empty());

    // The buffers are reusable across user turns: a fresh append + flush after
    // the drain produces a new turn, not a leftover from the cleared batch.
    h.handle.append_harness_trace_items(pair("turn-2"));
    h.handle.flush_harness_trace_turn();
    let reused = h.handle.take_harness_trace_turns().await;
    assert_eq!(reused.len(), 1, "append-after-drain seals a fresh turn");
    assert_eq!(reused[0].len(), 2);
}

#[tokio::test]
async fn flush_without_items_is_noop() {
    let h = TestHarness::new();
    // Fail-open verifier path (no skeptics spawned) flushes nothing.
    h.handle.flush_harness_trace_turn();
    let harness = h.handle.take_harness_trace_turns().await;
    assert!(harness.is_empty());
}

#[tokio::test]
async fn turn_capture_survives_compaction_and_flags_it() {
    let h = TestHarness::new();

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));

    h.handle
        .replace_conversation_for_compaction(vec![ConversationItem::system("compacted")]);

    h.handle.push_user_message(ConversationItem::user("q2"));

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture was active");

    assert_eq!(capture.messages.len(), 3);
    assert!(matches!(&capture.messages[0], ConversationItem::User(_)));
    assert!(matches!(
        &capture.messages[1],
        ConversationItem::Assistant(_)
    ));
    assert!(matches!(&capture.messages[2], ConversationItem::User(_)));
    assert!(capture.compaction_occurred);
}

#[tokio::test]
async fn replace_conversation_without_compaction_does_not_set_flag() {
    let h = TestHarness::new();

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("q1"));

    h.handle
        .replace_conversation(vec![ConversationItem::system("new state")]);

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture was active");

    assert!(!capture.compaction_occurred);
}

#[tokio::test]
async fn take_without_begin_returns_none() {
    let h = TestHarness::new();

    h.handle.push_user_message(ConversationItem::user("hello"));

    let result = h.handle.take_turn_messages().await;
    assert!(result.is_none());
}

#[tokio::test]
async fn take_twice_returns_none_second_time() {
    let h = TestHarness::new();

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("hello"));

    let first = h.handle.take_turn_messages().await;
    assert!(first.is_some());

    let second = h.handle.take_turn_messages().await;
    assert!(second.is_none());
}

#[tokio::test]
async fn begin_capture_clears_previous_buffer() {
    let h = TestHarness::new();

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("old"));

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("new"));

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture was active");

    assert_eq!(capture.messages.len(), 1);
    assert!(matches!(&capture.messages[0], ConversationItem::User(_)));
}

#[tokio::test]
async fn truncate_clears_turn_capture() {
    let h = TestHarness::new();

    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle.increment_prompt_index();
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("q2"));
    h.handle.increment_prompt_index();

    h.handle.truncate_to_prompt_index(1).await;

    let result = h.handle.take_turn_messages().await;
    assert!(result.is_none());
}

#[tokio::test]
async fn restore_snapshot_does_not_touch_turn_capture() {
    let h = TestHarness::new();

    let snapshot = h.handle.snapshot().await.unwrap();

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("hello"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("hi"));

    h.handle.restore_snapshot(snapshot);

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture should survive restore");

    assert_eq!(capture.messages.len(), 2);
    assert!(!capture.compaction_occurred);
}

#[tokio::test]
async fn turn_capture_survives_integrity_repair_prefix_shrink() {
    use echo_agent_sampling_types::ToolCall;
    let h = TestHarness::new();

    // Build a prefix (before the capture starts) holding three removable
    // duplicate ToolResults — one per tool call. `dedup_duplicate_tool_results`
    // keeps the last result per id and drops the earlier one, shrinking the
    // prefix by three items when integrity repair later runs.
    let call = |id: &'static str| ToolCall {
        id: id.into(),
        name: "t".into(),
        arguments: "{}".into(),
    };
    h.handle
        .push_assistant_response(ConversationItem::assistant_tool_calls(vec![
            call("call-1"),
            call("call-2"),
            call("call-3"),
        ]));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-1", "dup-1"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-1", "real-1"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-2", "dup-2"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-2", "real-2"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-3", "dup-3"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("call-3", "real-3"));

    // Capture starts after the 7-item prefix: turn_start_offset == 7.
    h.handle.begin_turn_capture();

    // First turn item lands while the prefix duplicates are still present.
    h.handle
        .push_assistant_response(ConversationItem::assistant("turn-1"));

    // Integrity repair removes the three prefix duplicates, shrinking the
    // conversation to len 5 while the un-rebased offset stays at 7 (offset 7 >
    // len 5). Without the fix the later take_turn_messages slice is out of range,
    // panics the actor, and the query comes back as None.
    h.handle.repair_dangling_after_harness_halt("test-halt");

    // Second turn item lands after the rebase — it must still be captured.
    h.handle
        .push_assistant_response(ConversationItem::assistant("turn-2"));

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture survived the in-place integrity repair (no actor panic)");

    // turn-1 via the pre-repair snapshot, turn-2 via the rebased offset; none of
    // the deduped prefix items leak in.
    assert_eq!(capture.messages.len(), 2);
    assert!(matches!(
        &capture.messages[0],
        ConversationItem::Assistant(a) if a.content.as_ref() == "turn-1" && a.tool_calls.is_empty()
    ));
    assert!(matches!(
        &capture.messages[1],
        ConversationItem::Assistant(a) if a.content.as_ref() == "turn-2" && a.tool_calls.is_empty()
    ));
}

#[tokio::test]
async fn integrity_repair_does_not_flag_compaction() {
    let h = TestHarness::new();

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));

    // An in-place integrity repair goes through `snapshot_turn_slice` like
    // compaction does, but it is NOT compaction — the flag must stay unset.
    h.handle.repair_dangling_after_harness_halt("test-halt");

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture was active");

    assert!(!capture.compaction_occurred);
    assert_eq!(capture.messages.len(), 2);
}

#[tokio::test]
async fn turn_capture_survives_persisted_memory_reminder_prepend() {
    // No leading System item, so the persisted memory inject prepends one via
    // `items.insert(0, ..)`, shifting every index by one under an active capture.
    let h = TestHarness::with_conversation(vec![ConversationItem::user("hi")]);

    h.handle.begin_turn_capture();
    h.handle.push_user_message(ConversationItem::user("turn-q"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("turn-a"));

    // Persist path injects into the LIVE conversation; without the snapshot +
    // rebase the prepend shifts every index but not the offset, so the off-by-one
    // tail over-reads past the turn boundary (the len check below catches it).
    let request = h
        .handle
        .build_request(
            vec![],
            Some("Remember this".to_string()),
            true,
            None,
            "c".into(),
            "r".into(),
        )
        .await
        .unwrap();
    assert!(matches!(&request.items[0], ConversationItem::System(_)));

    let capture = h
        .handle
        .take_turn_messages()
        .await
        .expect("capture was active");

    // Exactly the two turn items, in order.
    assert_eq!(capture.messages.len(), 2);
    assert!(matches!(&capture.messages[0], ConversationItem::User(_)));
    assert!(matches!(
        &capture.messages[1],
        ConversationItem::Assistant(a) if a.content.as_ref() == "turn-a"
    ));
}

// ============================================================================
// Narrow targeted query tests
// ============================================================================

#[tokio::test]
async fn get_conversation_len_empty() {
    let h = TestHarness::new();
    assert_eq!(h.handle.get_conversation_len().await, 0);
}

#[tokio::test]
async fn get_conversation_len_matches_full_conversation() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("a"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("b"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("c1", "r"));

    // Use len query as sync point and verify it matches the full vec
    let len = h.handle.get_conversation_len().await;
    let full = h.handle.get_conversation().await;
    assert_eq!(len, full.len());
    assert_eq!(len, 3);
}

#[tokio::test]
async fn has_dangling_tool_calls_reflects_unanswered_calls() {
    use echo_agent_sampling_types::ToolCall;
    let h = TestHarness::new();
    assert!(!h.handle.has_dangling_tool_calls().await);

    // `push_assistant_response` does not run the repair, so the unanswered call
    // stays dangling (mirrors a turn parked mid-tool / on a permission prompt).
    h.handle
        .push_assistant_response(ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "my_tool".to_string(),
            arguments: "{}".into(),
        }]));
    assert!(h.handle.has_dangling_tool_calls().await);

    // Answering the call clears the dangling state.
    h.handle
        .push_tool_result(ConversationItem::tool_result("call_1", "ok"));
    assert!(!h.handle.has_dangling_tool_calls().await);
}

#[tokio::test]
async fn get_last_assistant_text_empty_conversation() {
    let h = TestHarness::new();
    assert!(h.handle.get_last_assistant_text().await.is_none());
}

#[tokio::test]
async fn get_last_assistant_text_returns_last_nonempty() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("first answer"));
    h.handle.push_user_message(ConversationItem::user("q2"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("second answer"));

    let text = h.handle.get_last_assistant_text().await;
    assert_eq!(text.as_deref(), Some("second answer"));
}

#[tokio::test]
async fn get_last_assistant_text_skips_whitespace_only() {
    let h = TestHarness::new();
    h.handle
        .push_assistant_response(ConversationItem::assistant("real answer"));
    // Push a whitespace-only assistant message after the real one
    h.handle
        .push_assistant_response(ConversationItem::assistant("   \n  "));

    let text = h.handle.get_last_assistant_text().await;
    // Must skip the whitespace-only entry and return the previous one
    assert_eq!(text.as_deref(), Some("real answer"));
}

#[tokio::test]
async fn get_last_assistant_text_in_turn_stops_at_boundary() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("previous turn answer"));
    h.handle.push_user_message(ConversationItem::user("q2"));

    assert!(h.handle.get_last_assistant_text_in_turn().await.is_none());
    assert_eq!(
        h.handle.get_last_assistant_text().await.as_deref(),
        Some("previous turn answer"),
        "the unbounded sibling still sees prior turns"
    );
}

#[tokio::test]
async fn get_last_assistant_text_in_turn_walks_past_synthetic_injections() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("q"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("turn answer"));
    h.handle
        .push_user_message(ConversationItem::stop_hook_feedback("keep working"));

    assert_eq!(
        h.handle.get_last_assistant_text_in_turn().await.as_deref(),
        Some("turn answer"),
        "synthetic mid-turn items must not act as turn boundaries"
    );

    // A turn-starting synthetic item (auto-wake) IS a boundary.
    h.handle
        .push_user_message(ConversationItem::task_completed("task done"));
    assert!(h.handle.get_last_assistant_text_in_turn().await.is_none());
}

#[tokio::test]
async fn get_last_assistant_text_no_assistant_messages() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("hi"));
    assert!(h.handle.get_last_assistant_text().await.is_none());
}

#[tokio::test]
async fn get_first_user_text_empty_conversation() {
    let h = TestHarness::new();
    assert!(h.handle.get_first_user_text().await.is_none());
}

#[tokio::test]
async fn get_first_user_text_returns_first_user_text() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle
        .push_user_message(ConversationItem::user("hello world"));
    h.handle.push_user_message(ConversationItem::user("second"));

    let text = h.handle.get_first_user_text().await;
    assert_eq!(text.as_deref(), Some("hello world"));
}

#[tokio::test]
async fn get_first_user_text_no_user_messages() {
    let h = TestHarness::new();
    h.handle
        .push_user_message(ConversationItem::system("sys only"));

    assert!(h.handle.get_first_user_text().await.is_none());
}

#[tokio::test]
async fn get_conversation_item_at_out_of_bounds() {
    let h = TestHarness::new();
    assert!(h.handle.get_conversation_item_at(0).await.is_none());
    assert!(h.handle.get_conversation_item_at(99).await.is_none());
}

#[tokio::test]
async fn get_conversation_item_at_returns_correct_item() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle.push_user_message(ConversationItem::user("hello"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("hi"));

    // Sync point
    let _ = h.handle.get_conversation_len().await;

    let item0 = h.handle.get_conversation_item_at(0).await.unwrap();
    assert!(matches!(item0, ConversationItem::System(_)));

    let item1 = h.handle.get_conversation_item_at(1).await.unwrap();
    assert!(matches!(item1, ConversationItem::User(_)));

    let item2 = h.handle.get_conversation_item_at(2).await.unwrap();
    assert!(matches!(item2, ConversationItem::Assistant(_)));

    assert!(h.handle.get_conversation_item_at(3).await.is_none());
}

#[tokio::test]
async fn get_conversation_item_at_does_not_mutate_state() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle.push_user_message(ConversationItem::user("q"));

    // Fetching item[1] should not change what get_conversation returns
    let _ = h.handle.get_conversation_item_at(1).await;

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 2);
}

// ── Multimodal regression tests for get_first_user_text() ────────────────────

/// Confirms that `get_first_user_text()` returns `None` when the first content
/// part of the first user message is an image (not text). This preserves the
/// original call-site semantics used by the memory-search path.
#[tokio::test]
async fn get_first_user_text_image_first_returns_none() {
    use echo_agent_sampling_types::{ContentPart, UserItem};

    let h = TestHarness::new();
    // First message: image-only user message (no text part)
    h.handle.push_user_message(ConversationItem::User(UserItem {
        content: vec![ContentPart::Image {
            url: "data:image/png;base64,abc".into(),
        }],
        synthetic_reason: None,
        ..Default::default()
    }));

    // Must return None — first content part is not Text.
    assert!(h.handle.get_first_user_text().await.is_none());
}

/// Confirms that when the first user message has an image first and text second,
/// `get_first_user_text()` still returns `None` (first-part-is-text semantics).
#[tokio::test]
async fn get_first_user_text_image_then_text_returns_none() {
    use echo_agent_sampling_types::{ContentPart, UserItem};

    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::User(UserItem {
        content: vec![
            ContentPart::Image {
                url: "data:image/png;base64,abc".into(),
            },
            ContentPart::Text {
                text: "describe this image".into(),
            },
        ],
        synthetic_reason: None,
        ..Default::default()
    }));

    // Still None — first part is an image, so we do not fall through to the text part.
    assert!(h.handle.get_first_user_text().await.is_none());
}

/// Confirms the happy path for text-first multimodal messages.
#[tokio::test]
async fn get_first_user_text_text_then_image_returns_text() {
    use echo_agent_sampling_types::{ContentPart, UserItem};

    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::User(UserItem {
        content: vec![
            ContentPart::Text {
                text: "look at this".into(),
            },
            ContentPart::Image {
                url: "data:image/png;base64,abc".into(),
            },
        ],
        synthetic_reason: None,
        ..Default::default()
    }));

    let text = h.handle.get_first_user_text().await;
    assert_eq!(text.as_deref(), Some("look at this"));
}

// ── Tests for GetLastUserQueryText, GetConversationCounts, GetSystemMessage ───

#[tokio::test]
async fn get_last_user_query_text_empty_conversation() {
    let h = TestHarness::new();
    assert!(h.handle.get_last_user_query_text().await.is_none());
}

#[tokio::test]
async fn get_last_user_query_text_returns_last() {
    let h = TestHarness::new();
    h.handle
        .push_user_message(ConversationItem::user("first question"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("answer"));
    h.handle
        .push_user_message(ConversationItem::user("second question"));

    let text = h.handle.get_last_user_query_text().await;
    assert_eq!(text.as_deref(), Some("second question"));
}

#[tokio::test]
async fn get_conversation_counts_empty() {
    let h = TestHarness::new();
    let counts = h.handle.get_conversation_counts().await;
    assert_eq!(counts.total, 0);
    assert_eq!(counts.user, 0);
    assert_eq!(counts.assistant, 0);
    assert_eq!(counts.tool_result, 0);
}

#[tokio::test]
async fn get_conversation_counts_mixed() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("c1", "r1"));
    h.handle.push_user_message(ConversationItem::user("q2"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("a2"));

    let counts = h.handle.get_conversation_counts().await;
    assert_eq!(counts.total, 6);
    assert_eq!(counts.user, 2);
    assert_eq!(counts.assistant, 2);
    assert_eq!(counts.tool_result, 1);
}

#[tokio::test]
async fn get_system_message_none_when_absent() {
    let h = TestHarness::new();
    h.handle.push_user_message(ConversationItem::user("no sys"));
    assert!(h.handle.get_system_message().await.is_none());
}

#[tokio::test]
async fn get_system_message_returns_first_system() {
    let h = TestHarness::new();
    h.handle
        .push_user_message(ConversationItem::system("You are helpful."));
    h.handle.push_user_message(ConversationItem::user("hi"));

    let sys = h.handle.get_system_message().await.unwrap();
    assert!(matches!(sys, ConversationItem::System(s) if s.content.as_ref() == "You are helpful."));
}

// ============================================================================
// Subagent bootstrap regression tests
//
// These verify that `replace_conversation` correctly syncs the system prompt
// into a ChatStateActor that was spawned before the prompt was built — the
// exact sequence used by `spawn_session_actor` for subagents.
// ============================================================================

#[tokio::test]
async fn fresh_subagent_bootstrap_has_system_message_after_replace() {
    // Simulate a fresh (non-forked) subagent: actor starts with an empty conversation.
    let h = TestHarness::new(); // spawns with vec![]

    // At this point the actor has no system message, mirroring the bug.
    assert!(h.handle.get_system_message().await.is_none());

    // Build the system prompt and inject it (mirrors acp_session.rs lines 1287-1295).
    let system_prompt = "You are a helpful subagent.".to_string();
    let conversation = vec![ConversationItem::system(system_prompt.clone())];
    h.handle.replace_conversation(conversation);

    // After the sync the system message must be available for compaction.
    let sys = h.handle.get_system_message().await.unwrap();
    assert!(
        matches!(&sys, ConversationItem::System(s) if s.content.as_ref() == system_prompt),
        "expected system prompt after replace_conversation, got {sys:?}"
    );
}

#[tokio::test]
async fn forked_subagent_bootstrap_replaces_parent_system_message() {
    let parent_prompt = "You are the parent agent.".to_string();
    let child_prompt = "You are the child subagent.".to_string();

    // Simulate a forked subagent: actor starts with the parent's conversation
    // which already contains the parent's system message.
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system(parent_prompt.clone()),
        ConversationItem::user("hello"),
        ConversationItem::assistant("hi"),
    ]);

    // Verify the parent's system message is present initially.
    let sys = h.handle.get_system_message().await.unwrap();
    assert!(matches!(&sys, ConversationItem::System(s) if s.content.as_ref() == parent_prompt));

    // Build the child's system prompt and replace the first System item
    // (mirrors acp_session.rs lines 1287-1295).
    let mut conversation = vec![
        ConversationItem::system(parent_prompt),
        ConversationItem::user("hello"),
        ConversationItem::assistant("hi"),
    ];
    if let Some(ConversationItem::System(sys)) = conversation.first_mut() {
        sys.content = child_prompt.clone().into();
    }
    h.handle.replace_conversation(conversation);

    // The system message must now be the child's prompt, not the parent's.
    let sys = h.handle.get_system_message().await.unwrap();
    assert!(
        matches!(&sys, ConversationItem::System(s) if s.content.as_ref() == child_prompt),
        "expected child prompt after replace_conversation, got {sys:?}"
    );

    // The rest of the conversation must be preserved.
    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 3);
}

// ============================================================================
// In-memory retained pruning tests (PR3)
// ============================================================================

/// Helper: push N complete turns (user + assistant + tool-result) so the
/// conversation grows to a predictable length.
async fn push_turns(handle: &crate::handle::ChatStateHandle, turns: usize, content_len: usize) {
    for i in 0..turns {
        handle.push_user_message(ConversationItem::user(format!("q{i}")));
        handle.increment_prompt_index();
        handle.push_assistant_response(ConversationItem::assistant(format!("a{i}")));
        handle.push_tool_result(ConversationItem::tool_result(
            format!("call_{i}"),
            "x".repeat(content_len),
        ));
    }
    // Sync point
    let _ = handle.get_conversation_len().await;
}

/// After fewer turns than `hard_clear_age_turns` the retained conversation
/// must not be touched (fast-exit path).
#[tokio::test]
async fn prune_retained_no_op_when_session_is_young() {
    use crate::actor::ChatStateActor;
    use crate::persistence::MockChatPersistence;
    use crate::types::PruningConfig;

    let (mock, _rx) = MockChatPersistence::new();
    let (event_tx, _) = tokio::sync::mpsc::unbounded_channel();
    let token = tokio_util::sync::CancellationToken::new();
    let config = PruningConfig {
        hard_clear_age_turns: 10,
        ..Default::default()
    };
    let handle = ChatStateActor::spawn_with_pruning(
        vec![],
        test_config(),
        config,
        Box::new(mock),
        event_tx,
        token,
    );

    // Push 5 turns — below the hard_clear_age_turns threshold of 10.
    push_turns(&handle, 5, 10_000).await;

    // All tool results must be untouched.
    let conv = handle.get_conversation().await;
    for item in &conv {
        if let ConversationItem::ToolResult(tr) = item {
            assert_eq!(
                tr.content.len(),
                10_000,
                "young session: tool result must not be pruned"
            );
        }
    }
}

/// After `hard_clear_age_turns + 1` turns, tool results from the oldest
/// turns must be hard-cleared in the in-memory conversation.
#[tokio::test]
async fn prune_retained_hard_clears_old_tool_results() {
    use crate::actor::ChatStateActor;
    use crate::persistence::MockChatPersistence;
    use crate::types::PruningConfig;

    let (mock, _rx) = MockChatPersistence::new();
    let (event_tx, _) = tokio::sync::mpsc::unbounded_channel();
    let token = tokio_util::sync::CancellationToken::new();
    let config = PruningConfig {
        hard_clear_age_turns: 5,
        keep_last_n_turns: 2,
        ..Default::default()
    };
    let handle = ChatStateActor::spawn_with_pruning(
        vec![],
        test_config(),
        config,
        Box::new(mock),
        event_tx,
        token,
    );

    // Push 8 turns — turns 0..2 will be older than hard_clear_age_turns=5.
    push_turns(&handle, 8, 5_000).await;

    let conv = handle.get_conversation().await;
    // Turns are laid out as [User, Assistant, ToolResult] * 8.
    // ToolResult for turn 0 is at index 2.
    let oldest_tr = match &conv[2] {
        ConversationItem::ToolResult(tr) => tr.content.clone(),
        other => panic!("expected ToolResult at index 2, got {other:?}"),
    };
    assert_eq!(
        oldest_tr.as_ref(),
        "[Tool result omitted — too old]",
        "oldest tool result must be hard-cleared"
    );

    // Recent turns (6, 7) must be untouched.
    let recent_tr_6 = match &conv[6 * 3 + 2] {
        ConversationItem::ToolResult(tr) => tr.content.clone(),
        other => panic!("expected ToolResult, got {other:?}"),
    };
    assert_eq!(
        recent_tr_6.len(),
        5_000,
        "recent tool result must NOT be pruned"
    );
}

/// Pruning is a no-op when `enabled = false`.
#[tokio::test]
async fn prune_retained_disabled_is_noop() {
    use crate::actor::ChatStateActor;
    use crate::persistence::MockChatPersistence;
    use crate::types::PruningConfig;

    let (mock, _rx) = MockChatPersistence::new();
    let (event_tx, _) = tokio::sync::mpsc::unbounded_channel();
    let token = tokio_util::sync::CancellationToken::new();
    let config = PruningConfig {
        enabled: false,
        hard_clear_age_turns: 3,
        keep_last_n_turns: 1,
        ..Default::default()
    };
    let handle = ChatStateActor::spawn_with_pruning(
        vec![],
        test_config(),
        config,
        Box::new(mock),
        event_tx,
        token,
    );

    push_turns(&handle, 6, 10_000).await;

    let conv = handle.get_conversation().await;
    for item in &conv {
        if let ConversationItem::ToolResult(tr) = item {
            assert_eq!(
                tr.content.len(),
                10_000,
                "disabled pruning: tool result must not be pruned"
            );
        }
    }
}

/// Retained conversation size is bounded after many turns: old tool results
/// are replaced with a short placeholder, not retained in full.
#[tokio::test]
async fn prune_retained_bounds_long_session_footprint() {
    use crate::actor::ChatStateActor;
    use crate::persistence::MockChatPersistence;
    use crate::types::PruningConfig;

    const TURNS: usize = 50; // enough turns to clear many old tool results
    const CONTENT_LEN: usize = 50_000; // 50 KB per tool result
    const PLACEHOLDER_LEN: usize = "[Tool result omitted — too old]".len();

    let (mock, _rx) = MockChatPersistence::new();
    let (event_tx, _) = tokio::sync::mpsc::unbounded_channel();
    let token = tokio_util::sync::CancellationToken::new();
    let config = PruningConfig {
        hard_clear_age_turns: 5,
        keep_last_n_turns: 2,
        ..Default::default()
    };
    let handle = ChatStateActor::spawn_with_pruning(
        vec![],
        test_config(),
        config,
        Box::new(mock),
        event_tx,
        token,
    );

    push_turns(&handle, TURNS, CONTENT_LEN).await;

    let conv = handle.get_conversation().await;

    // Count how many tool results are still at full size vs. cleared.
    let (full, cleared) = conv.iter().fold((0usize, 0usize), |(f, c), item| {
        if let ConversationItem::ToolResult(tr) = item {
            if tr.content.len() == PLACEHOLDER_LEN {
                (f, c + 1)
            } else {
                (f + 1, c)
            }
        } else {
            (f, c)
        }
    });

    // Most tool results must be cleared; only the very recent ones (≤ keep_last_n_turns) are kept.
    assert!(
        cleared > full,
        "majority of old tool results must be hard-cleared (cleared={cleared}, full={full})"
    );

    // Approximate in-memory footprint of tool results should be much less than
    // the naive uncompressed size.
    let actual_tr_bytes: usize = conv
        .iter()
        .filter_map(|i| {
            if let ConversationItem::ToolResult(tr) = i {
                Some(tr.content.len())
            } else {
                None
            }
        })
        .sum();
    let naive_bytes = TURNS * CONTENT_LEN;
    assert!(
        actual_tr_bytes < naive_bytes / 5,
        "retained tool-result bytes ({actual_tr_bytes}) must be << naive ({naive_bytes})"
    );
}

/// Rewind (truncate_to_prompt_index) still produces the correct item count
/// after in-memory pruning has cleared some old tool results.
#[tokio::test]
async fn prune_retained_rewind_still_correct() {
    use crate::actor::ChatStateActor;
    use crate::persistence::MockChatPersistence;
    use crate::types::PruningConfig;

    let (mock, mut rx) = MockChatPersistence::new();
    let (event_tx, _) = tokio::sync::mpsc::unbounded_channel();
    let token = tokio_util::sync::CancellationToken::new();
    // Aggressive pruning to make in-memory pruning fire quickly.
    let config = PruningConfig {
        hard_clear_age_turns: 3,
        keep_last_n_turns: 1,
        ..Default::default()
    };
    let handle = ChatStateActor::spawn_with_pruning(
        vec![],
        test_config(),
        config,
        Box::new(mock),
        event_tx,
        token,
    );

    // Push 6 turns: [User, Assistant, ToolResult] * 6 = 18 items.
    push_turns(&handle, 6, 1_000).await;

    // Rewind to prompt index 3 (keep turns 0..3 = 9 items).
    handle.truncate_to_prompt_index(3).await;

    let conv = handle.get_conversation().await;
    assert_eq!(
        conv.len(),
        9,
        "after rewind to prompt_index=3, should have 9 items (3 turns * 3 items)"
    );
    let idx = handle.get_prompt_index().await;
    assert_eq!(idx, 3);

    // Verify we have the right item types: (User, Assistant, ToolResult) * 3.
    for turn in 0..3 {
        assert!(
            matches!(&conv[turn * 3], ConversationItem::User(_)),
            "item[{turn}*3] should be User"
        );
        assert!(
            matches!(&conv[turn * 3 + 1], ConversationItem::Assistant(_)),
            "item[{turn}*3+1] should be Assistant"
        );
        assert!(
            matches!(&conv[turn * 3 + 2], ConversationItem::ToolResult(_)),
            "item[{turn}*3+2] should be ToolResult"
        );
    }

    // Drain to avoid memory leak warning.
    rx.drain();
}

/// Regression test: synthetic `User` items injected mid-turn (e.g. system
/// warnings) must NOT cause old tool results to be cleared earlier than
/// `hard_clear_age_turns` real turns.
///
/// Scenario: 3 real turns with tool results, then 2 synthetic User items
/// injected without incrementing `prompt_index`, then a 4th real turn.
/// With `hard_clear_age_turns = 5`, none of the 4 tool results should be
/// cleared yet (the oldest is only 4 real turns old after the 4th real turn
/// starts, since prompt_index is 4 at prune time).
#[tokio::test]
async fn prune_retained_synthetic_user_does_not_advance_age() {
    use crate::actor::ChatStateActor;
    use crate::persistence::MockChatPersistence;
    use crate::types::PruningConfig;

    let (mock, _rx) = MockChatPersistence::new();
    let (event_tx, _) = tokio::sync::mpsc::unbounded_channel();
    let token = tokio_util::sync::CancellationToken::new();
    let config = PruningConfig {
        hard_clear_age_turns: 5,
        keep_last_n_turns: 1,
        ..Default::default()
    };
    let handle = ChatStateActor::spawn_with_pruning(
        vec![],
        test_config(),
        config,
        Box::new(mock),
        event_tx,
        token,
    );

    // Three real turns, each with a large tool result.
    for i in 0..3usize {
        handle.push_user_message(ConversationItem::user(format!("real q{i}")));
        handle.increment_prompt_index(); // prompt_index = i+1
        handle.push_assistant_response(ConversationItem::assistant(format!("a{i}")));
        handle.push_tool_result(ConversationItem::tool_result(
            format!("call_{i}"),
            "x".repeat(10_000),
        ));
    }

    // Two synthetic User items injected mid-turn (e.g. doom-loop warnings).
    // These do NOT call increment_prompt_index — prompt_index stays at 3.
    handle.push_user_message(ConversationItem::user("⚠️ doom-loop warning 1"));
    handle.push_user_message(ConversationItem::user("⚠️ doom-loop warning 2"));

    // Fourth real turn starts: prompt_index → 4, pruning fires inside push_user_message.
    handle.push_user_message(ConversationItem::user("real q3"));
    handle.increment_prompt_index(); // prompt_index = 4

    // Sync
    let conv = handle.get_conversation().await;

    // None of the 3 original tool results should be cleared:
    // oldest real age = 3 turns (turn 0 is 3 real turns ago), threshold = 5.
    // Without the synthetic-count compensation, the 2 synthetic User items
    // would make turn 0's TR appear age 5, causing premature clearing.
    for item in &conv {
        if let ConversationItem::ToolResult(tr) = item {
            assert_ne!(
                tr.content.as_ref(),
                "[Tool result omitted — too old]",
                "tool result must NOT be cleared: oldest is only 3 real turns old \
                 (threshold is 5), synthetic user injections must not advance age"
            );
        }
    }
}

#[tokio::test]
async fn get_last_model_metadata_returns_both_fields() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hi"),
        ConversationItem::Assistant(echo_agent_sampling_types::AssistantItem {
            content: "hello".into(),
            tool_calls: vec![],
            model_id: Some("echo-agent-4.5".into()),
            model_fingerprint: Some("fp_abc123".into()),
            reasoning_effort: None,
        }),
    ]);
    let meta = h.handle.get_last_model_metadata().await;
    assert_eq!(meta.resolved_model_id.as_deref(), Some("echo-agent-4.5"));
    assert_eq!(meta.model_fingerprint.as_deref(), Some("fp_abc123"));
}

#[tokio::test]
async fn get_last_model_metadata_returns_default_when_no_assistant() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hi"),
    ]);
    let meta = h.handle.get_last_model_metadata().await;
    assert!(meta.resolved_model_id.is_none());
    assert!(meta.model_fingerprint.is_none());
}

/// Reproduce: after compaction replaces the conversation, `get_sampling_config`
/// must still return the original model/context_window/api_backend. The
/// `SamplingConfig` lives in a separate field — `replace_conversation` must
/// not touch it.
#[tokio::test]
async fn sampling_config_survives_compaction_replacement() {
    use echo_agent_sampling_types::ApiBackend;

    let config = SamplingConfig {
        base_url: "https://api.example.com".to_string(),
        model: "echo-agent-build".to_string(),
        max_completion_tokens: None,
        temperature: Some(0.7),
        top_p: Some(0.95),
        api_backend: ApiBackend::Responses,
        extra_headers: Default::default(),
        query_params: Default::default(),
        env_http_headers: Default::default(),
        context_window: NonZeroU64::new(500_000).unwrap(),
        reasoning_effort: None,
        stream_tool_calls: None,
        supports_tool_calls: None,
    };

    let h = TestHarness::with_config(
        vec![
            ConversationItem::system("You are a helpful assistant."),
            ConversationItem::user("fix the bug"),
            ConversationItem::Assistant(echo_agent_sampling_types::AssistantItem {
                content: "I'll fix it.".into(),
                tool_calls: vec![],
                model_id: Some("echo-agent-4.5".into()),
                model_fingerprint: Some("fp_abc123".into()),
                reasoning_effort: None,
            }),
        ],
        config,
    );

    // Pre-compaction: everything correct.
    let pre = h.handle.get_sampling_config().await.unwrap();
    assert_eq!(pre.model, "echo-agent-build");
    assert_eq!(pre.context_window.get(), 500_000);
    assert_eq!(pre.api_backend, ApiBackend::Responses);

    let pre_meta = h.handle.get_last_model_metadata().await;
    assert_eq!(
        pre_meta.resolved_model_id.as_deref(),
        Some("echo-agent-4.5")
    );
    assert_eq!(pre_meta.model_fingerprint.as_deref(), Some("fp_abc123"));

    // Simulate compaction: replace conversation with compacted history.
    // The compacted history has system + user summary, NO AssistantItem.
    h.handle.replace_conversation_for_compaction(vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Compaction summary: user asked to fix a bug..."),
    ]);

    // Post-compaction: SamplingConfig MUST be preserved.
    let post = h.handle.get_sampling_config().await.unwrap();
    assert_eq!(
        post.model, "echo-agent-build",
        "BUG: model changed after compaction"
    );
    assert_eq!(
        post.context_window.get(),
        500_000,
        "BUG: context_window dropped to default after compaction"
    );
    assert_eq!(
        post.api_backend,
        ApiBackend::Responses,
        "BUG: api_backend switched to ChatCompletions after compaction"
    );

    // Post-compaction: model metadata is LOST (no AssistantItem in compacted history).
    // This is the visible symptom -- fingerprint/hash disappears from /session-info.
    let post_meta = h.handle.get_last_model_metadata().await;
    assert!(
        post_meta.resolved_model_id.is_none(),
        "resolved_model_id should be None after compaction (no AssistantItem)"
    );
    assert!(
        post_meta.model_fingerprint.is_none(),
        "model_fingerprint should be None after compaction (no AssistantItem)"
    );
}

/// After compaction, the `build_session_info` display path uses
/// `get_sampling_config().model` as the source-of-truth model slug.
/// If that model slug is e.g. "echo-agent-build" and not in the ModelState
/// catalog with a display name, the pager shows the raw slug. This
/// test verifies the pager's `current_model_name()` behavior when the
/// model ID doesn't match any catalog entry.
#[tokio::test]
async fn model_metadata_lost_after_compaction_then_recovered_on_next_turn() {
    let config = SamplingConfig {
        base_url: "https://api.example.com".to_string(),
        model: "echo-agent-build".to_string(),
        max_completion_tokens: None,
        temperature: Some(0.7),
        top_p: Some(0.95),
        api_backend: Default::default(),
        extra_headers: Default::default(),
        query_params: Default::default(),
        env_http_headers: Default::default(),
        context_window: NonZeroU64::new(500_000).unwrap(),
        reasoning_effort: None,
        stream_tool_calls: None,
        supports_tool_calls: None,
    };

    let h = TestHarness::with_config(
        vec![
            ConversationItem::system("sys"),
            ConversationItem::user("task"),
            ConversationItem::Assistant(echo_agent_sampling_types::AssistantItem {
                content: "done".into(),
                tool_calls: vec![],
                model_id: Some("echo-agent-4.5".into()),
                model_fingerprint: Some("fp_acd3142484d3ad6f".into()),
                reasoning_effort: None,
            }),
        ],
        config,
    );

    // Before compaction: metadata present.
    let meta = h.handle.get_last_model_metadata().await;
    assert_eq!(meta.resolved_model_id.as_deref(), Some("echo-agent-4.5"));
    assert_eq!(
        meta.model_fingerprint.as_deref(),
        Some("fp_acd3142484d3ad6f")
    );

    // Compaction replaces conversation — no AssistantItem in compacted history.
    h.handle.replace_conversation_for_compaction(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("compacted summary"),
    ]);

    // Metadata gone.
    let meta = h.handle.get_last_model_metadata().await;
    assert!(meta.resolved_model_id.is_none());
    assert!(meta.model_fingerprint.is_none());

    // Simulate next turn: model responds and metadata is restored.
    h.handle
        .push_user_message(ConversationItem::user("next task"));
    h.handle
        .push_assistant_response(ConversationItem::Assistant(
            echo_agent_sampling_types::AssistantItem {
                content: "working on it".into(),
                tool_calls: vec![],
                model_id: Some("echo-agent-4.5".into()),
                model_fingerprint: Some("fp_acd3142484d3ad6f".into()),
                reasoning_effort: None,
            },
        ));

    // Metadata recovered.
    let meta = h.handle.get_last_model_metadata().await;
    assert_eq!(meta.resolved_model_id.as_deref(), Some("echo-agent-4.5"));
    assert_eq!(
        meta.model_fingerprint.as_deref(),
        Some("fp_acd3142484d3ad6f")
    );
}

/// Verify that a context_window downgrade via `update_sampling_config`
/// causes `check_auto_compact_needed` to fire when token usage already
/// exceeds the new (smaller) window.
///
/// This exercises the actor's arithmetic: if anything (model switch,
/// session resume, etc.) shrinks the context window below accumulated
/// token usage, auto-compact must trigger.
///
/// Note: `handle_model_metadata_update` in acp_session.rs now blocks
/// response-header downgrades (only upgrades accepted), so this path
/// is mainly reachable via model switches. The actor itself still
/// accepts any value via `update_sampling_config` — the guard lives
/// in the session layer.
#[tokio::test]
async fn context_window_downgrade_triggers_auto_compact() {
    use echo_agent_sampling_types::ApiBackend;

    // Initial config: 500k context, Responses backend (matches echo-agent-4.5)
    let config = SamplingConfig {
        base_url: "https://api.echo-agent.invalid/v1".to_string(),
        model: "echo-agent-4.5".to_string(),
        max_completion_tokens: None,
        temperature: Some(0.7),
        top_p: Some(0.95),
        api_backend: ApiBackend::Responses,
        extra_headers: Default::default(),
        query_params: Default::default(),
        env_http_headers: Default::default(),
        context_window: NonZeroU64::new(500_000).unwrap(),
        reasoning_effort: None,
        stream_tool_calls: None,
        supports_tool_calls: None,
    };

    let h = TestHarness::with_config(vec![], config);

    // Simulate 217k tokens of conversation (matching turn 587's total_tokens)
    h.handle.record_token_usage(217_000);

    // Pre-downgrade: 217k / 500k = 43% — well under auto-compact threshold
    let pre = h.handle.get_sampling_config().await.unwrap();
    assert_eq!(pre.context_window.get(), 500_000);

    let trigger = h.handle.check_auto_compact_needed(85).await;
    assert!(
        trigger.is_none(),
        "should NOT trigger auto-compact at 43% (217k/500k)"
    );

    // Simulate a context_window downgrade (e.g. model switch, response
    // header from cli-chat-proxy, or stale prefetched model list).
    let mut downgraded = pre.clone();
    downgraded.context_window = NonZeroU64::new(128_000).unwrap();
    h.handle.update_sampling_config(downgraded);

    // Post-downgrade: 217k / 128k = 170% — massively over capacity
    let post = h.handle.get_sampling_config().await.unwrap();
    assert_eq!(
        post.context_window.get(),
        128_000,
        "context_window should be overwritten by update_sampling_config"
    );
    assert_eq!(post.model, "echo-agent-4.5", "model slug must not change");
    assert_eq!(
        post.api_backend,
        ApiBackend::Responses,
        "api_backend must not change"
    );

    // Now auto-compact sees the 128k window and fires
    let trigger = h.handle.check_auto_compact_needed(85).await;
    assert!(
        trigger.is_some(),
        "MUST trigger auto-compact at 170% (217k/128k) — this is the bug!"
    );
    let info = trigger.unwrap();
    assert_eq!(info.context_window, NonZeroU64::new(128_000).unwrap());
    assert_eq!(info.total_tokens, 217_000);
    // utilization_percent is u8 so it caps at 255, but we just need >85
    assert!(
        info.utilization_percent > 85,
        "utilization should be well above threshold but got {}%",
        info.utilization_percent
    );
}

// ============================================================================
// KV Cache Prefix Stability Tests
//
// These test `build_conversation_request()` output prefix stability through
// the full pipeline -- pruning, memory injection, image pruning, snapshot
// restore. Prefix stability within a compaction epoch is the invariant that
// keeps the inference engine's prefix / KV cache hitting. The sibling-Reasoning refactor
// deleted the placeholder/splice machinery these tests previously had to work
// around.
//
// These target the refactored sibling-Reasoning shape:
//   - No `__RAW_OUTPUT_PLACEHOLDER__` sentinels
//   - No `extract_raw_input_items()` / `splice_raw_input_items()`
//   - Reasoning lives as `ConversationItem::Reasoning(rs::ReasoningItem)`
//     siblings; the From<&ConversationRequest> for rs::CreateResponse impl
//     emits them inline in `input` order.
// ============================================================================

/// Serialize a ConversationRequest using only the public
/// `From<&ConversationRequest> for rs::CreateResponse` trait impl.
///
/// After the sibling-Reasoning refactor there is no placeholder/splice dance: the `Vec<rs::InputItem>`
/// produced by the From impl is directly the wire shape (modulo
/// `patch_reasoning_text_types` which only stamps a `type` field on nested
/// reasoning content blocks and does not reorder items).
fn serialize_via_public_api(
    req: &echo_agent_sampling_types::ConversationRequest,
) -> serde_json::Value {
    use echo_agent_sampling_types::rs;
    let create_response: rs::CreateResponse = req.into();
    let mut body = serde_json::to_value(&create_response).unwrap();
    echo_agent_sampling_types::patch_reasoning_text_types(&mut body);
    // Sanity guard: the placeholder string from the pre-refactor design
    // must never appear in the serialized output. If a future change
    // re-introduces a stringly-typed splice, this catches it.
    let body_str = serde_json::to_string(&body).unwrap();
    assert!(
        !body_str.contains("__RAW_OUTPUT_PLACEHOLDER_"),
        "placeholder sentinel must never appear in serialized output \
         post-sibling-Reasoning refactor"
    );
    body
}

/// Assert that request N's serialized input is a byte-stable prefix of
/// request N+1's.
fn assert_prefix_stable_pair(
    base: &echo_agent_sampling_types::ConversationRequest,
    extended: &echo_agent_sampling_types::ConversationRequest,
    label: &str,
) {
    let base_body = serialize_via_public_api(base);
    let ext_body = serialize_via_public_api(extended);

    let base_input = base_body["input"].as_array().unwrap();
    let ext_input = ext_body["input"].as_array().unwrap();

    assert!(
        ext_input.len() >= base_input.len(),
        "{label}: extended request has fewer input items ({}) than base ({})",
        ext_input.len(),
        base_input.len(),
    );
    assert_eq!(
        &ext_input[..base_input.len()],
        base_input.as_slice(),
        "{label}: prefix broken. Base has {} items, extended has {}. \
         First divergence at index {}",
        base_input.len(),
        ext_input.len(),
        base_input
            .iter()
            .zip(ext_input.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(base_input.len()),
    );
}

/// Build a sibling Reasoning item with the given id and (optionally)
/// encrypted_content. Replaces the pre-refactor "AssistantItem.raw_output"
/// helper.
fn reasoning_sibling(id: &str, encrypted: Option<&str>) -> ConversationItem {
    use echo_agent_sampling_types::rs;
    ConversationItem::Reasoning(rs::ReasoningItem {
        id: id.to_string(),
        summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
            text: format!("thinking for {id}"),
        })],
        content: None,
        encrypted_content: encrypted.map(str::to_owned),
        status: None,
    })
}

/// Basic multi-turn prefix stability through build_request().
/// Each turn adds a user message + assistant response; the serialized
/// input from turn N must be a prefix of turn N+1.
#[tokio::test]
async fn prefix_stable_across_user_assistant_turns() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Hello"),
    ]);

    let req1 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r1".into())
        .await
        .unwrap();

    h.handle
        .push_assistant_response(ConversationItem::assistant("Hi there!"));
    h.handle
        .push_user_message(ConversationItem::user("How are you?"));

    let req2 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r2".into())
        .await
        .unwrap();

    assert_prefix_stable_pair(&req1, &req2, "turn 1 -> turn 2");

    h.handle
        .push_assistant_response(ConversationItem::assistant("I'm well!"));
    h.handle.push_user_message(ConversationItem::user("Great"));

    let req3 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r3".into())
        .await
        .unwrap();

    assert_prefix_stable_pair(&req2, &req3, "turn 2 -> turn 3");
}

/// Prefix stability when memory reminders are injected.
/// Once a memory reminder is established, subsequent requests with the
/// SAME reminder must produce stable prefixes.
#[tokio::test]
async fn prefix_stable_with_consistent_memory_injection() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("You are helpful."),
        ConversationItem::user("Hello"),
    ]);

    let req1 = h
        .handle
        .build_request(
            vec![],
            Some("Remember: user likes Rust".to_string()),
            false,
            None,
            "c".into(),
            "r1".into(),
        )
        .await
        .unwrap();

    h.handle
        .push_assistant_response(ConversationItem::assistant("Hi!"));
    h.handle
        .push_user_message(ConversationItem::user("Tell me more"));

    let req2 = h
        .handle
        .build_request(
            vec![],
            Some("Remember: user likes Rust".to_string()),
            false,
            None,
            "c".into(),
            "r2".into(),
        )
        .await
        .unwrap();

    assert_prefix_stable_pair(&req1, &req2, "memory-injected turn 1 -> turn 2");
}

/// Prefix stability with Reasoning siblings (encrypted reasoning) through
/// the full build_request pipeline. This is the structural equivalent of the
/// earlier `prefix_stable_with_raw_output_through_build_request` test --
/// it exercises the exact code path that caused a prefix-instability incident,
/// but on the post-refactor data model where reasoning rides as a typed sibling
/// rather than an `AssistantItem.raw_output` blob.
#[tokio::test]
async fn prefix_stable_with_reasoning_siblings_through_build_request() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("u1"),
    ]);

    let req1 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r1".into())
        .await
        .unwrap();

    // Push Reasoning sibling + Assistant (the new ordering produced by
    // `response_to_conversation_items`: Reasoning before Assistant).
    h.handle
        .push_tool_result(reasoning_sibling("r_abc", Some("enc1")));
    h.handle
        .push_assistant_response(ConversationItem::assistant("response 1"));
    h.handle.push_user_message(ConversationItem::user("u2"));

    let req2 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r2".into())
        .await
        .unwrap();

    assert_prefix_stable_pair(&req1, &req2, "Reasoning sibling turn 1 -> turn 2");

    // Push another turn's Reasoning + Assistant
    h.handle
        .push_tool_result(reasoning_sibling("r_def", Some("enc2")));
    h.handle
        .push_assistant_response(ConversationItem::assistant("response 2"));
    h.handle.push_user_message(ConversationItem::user("u3"));

    let req3 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r3".into())
        .await
        .unwrap();

    assert_prefix_stable_pair(
        &req2,
        &req3,
        "Reasoning sibling turn 2 -> turn 3 (cross-turn prefix-stability regression)",
    );
}

/// Tool definitions are in the `tools` field, not the `input` array.
/// Changing the tool set between requests must not affect the input prefix.
#[tokio::test]
async fn prefix_stable_after_tool_schema_change() {
    use echo_agent_sampling_types::ToolSpec;

    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hello"),
    ]);

    let tools_v1 = vec![ToolSpec {
        name: "read_file".to_string(),
        description: Some("Read a file".to_string()),
        parameters: serde_json::json!({"type": "object"}),
    }];

    let req1 = h
        .handle
        .build_request(tools_v1, None, false, None, "c".into(), "r1".into())
        .await
        .unwrap();

    h.handle
        .push_assistant_response(ConversationItem::assistant("read it"));
    h.handle
        .push_user_message(ConversationItem::user("now edit"));

    let tools_v2 = vec![
        ToolSpec {
            name: "read_file".to_string(),
            description: Some("Read a file".to_string()),
            parameters: serde_json::json!({"type": "object"}),
        },
        ToolSpec {
            name: "edit_file".to_string(),
            description: Some("Edit a file".to_string()),
            parameters: serde_json::json!({"type": "object"}),
        },
    ];

    let req2 = h
        .handle
        .build_request(tools_v2, None, false, None, "c".into(), "r2".into())
        .await
        .unwrap();

    assert_prefix_stable_pair(&req1, &req2, "tool schema v1 -> v2");
}

/// Model switch between turns: model is in request metadata, not input
/// items. Input prefix must remain stable.
#[tokio::test]
async fn prefix_stable_after_model_switch() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hello"),
    ]);

    let req1 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r1".into())
        .await
        .unwrap();

    h.handle
        .push_assistant_response(ConversationItem::assistant("hi"));
    h.handle
        .push_user_message(ConversationItem::user("continue"));

    let new_config = SamplingConfig {
        model: "echo-agent-3-mini".to_string(),
        ..test_config()
    };
    h.handle.update_sampling_config(new_config);

    let req2 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r2".into())
        .await
        .unwrap();

    assert_prefix_stable_pair(&req1, &req2, "model switch");
}

/// Synthetic user messages (doom loop warning, auto-continue) are
/// appended -- not inserted before existing items -- so the prefix
/// must remain stable.
#[tokio::test]
async fn prefix_stable_with_synthetic_user_messages() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hello"),
    ]);

    let req1 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r1".into())
        .await
        .unwrap();

    h.handle
        .push_assistant_response(ConversationItem::assistant("hi"));

    h.handle
        .push_user_message(ConversationItem::system_reminder("stop looping"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("ok"));
    h.handle
        .push_user_message(ConversationItem::auto_continue("keep going"));

    let req2 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r2".into())
        .await
        .unwrap();

    assert_prefix_stable_pair(&req1, &req2, "with synthetic user messages");
}

/// Prefix stability after size-gated image eviction. Once the serialized body
/// nears the 50 MB ceiling, old user turns' images are replaced with text
/// placeholders on the request clone -- text items before the evicted region
/// must stay prefix-stable in their relative ordering. The image here is sized
/// past `IMAGE_COMPACT_TRIGGER_BYTES` so the eviction actually fires.
#[tokio::test]
async fn prefix_stable_after_image_pruning() {
    use echo_agent_sampling_types::ContentPart;

    // Large enough that the serialized body crosses the compaction trigger.
    let big_image_url = format!(
        "data:image/png;base64,{}",
        "A".repeat(crate::image_budget::IMAGE_COMPACT_TRIGGER_BYTES)
    );

    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::User(echo_agent_sampling_types::UserItem {
            content: vec![
                ContentPart::Text {
                    text: "look at this image".into(),
                },
                ContentPart::Image {
                    url: big_image_url.into(),
                },
            ],
            synthetic_reason: None,
            ..Default::default()
        }),
        ConversationItem::assistant("I see it"),
        ConversationItem::user("u2"),
    ]);

    let req1 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r1".into())
        .await
        .unwrap();

    h.handle
        .push_assistant_response(ConversationItem::assistant("a2"));
    h.handle.push_user_message(ConversationItem::User(
        echo_agent_sampling_types::UserItem {
            content: vec![
                ContentPart::Text {
                    text: "new image".into(),
                },
                ContentPart::Image {
                    url: "data:image/png;base64,newImageData".into(),
                },
            ],
            synthetic_reason: None,
            ..Default::default()
        },
    ));

    let req2 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r2".into())
        .await
        .unwrap();

    // Image stripping mutates the old user turn's content, so full
    // byte-level prefix stability cannot hold at that item. We verify:
    //   1. System prompt preserved
    //   2. Items grew
    //   3. Text items appear in the same relative order
    let body1 = serialize_via_public_api(&req1);
    let body2 = serialize_via_public_api(&req2);

    let input1 = body1["input"].as_array().unwrap();
    let input2 = body2["input"].as_array().unwrap();

    assert_eq!(
        input1[0], input2[0],
        "system prompt must be preserved after image pruning"
    );
    assert!(
        input2.len() > input1.len(),
        "extended request must have more items"
    );

    let extract_text_items = |input: &[serde_json::Value]| -> Vec<String> {
        input
            .iter()
            .filter_map(|v| {
                let role = v.get("role").and_then(|r| r.as_str())?;
                let content = v.get("content").and_then(|c| c.as_str())?;
                Some(format!("{role}:{content}"))
            })
            .collect()
    };
    let texts1 = extract_text_items(input1);
    let texts2 = extract_text_items(input2);
    let mut idx2 = 0;
    for t1 in &texts1 {
        while idx2 < texts2.len() && &texts2[idx2] != t1 {
            idx2 += 1;
        }
        assert!(
            idx2 < texts2.len(),
            "text item {t1:?} from req1 must appear in req2 in the same order"
        );
        idx2 += 1;
    }
}

/// Regression for the image cache-miss bug: with normal small images (well
/// under the 50 MB ceiling), an old user turn's image is preserved across
/// turns instead of being rewritten to a placeholder. Rewriting old images on
/// every turn busted the KV-cache prefix (the over-aggressive earlier
/// behavior this size-gate replaces).
#[tokio::test]
async fn build_request_preserves_small_old_images() {
    use echo_agent_sampling_types::{ContentPart, UserItem};

    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::User(UserItem {
            content: vec![
                ContentPart::Text {
                    text: "look at this".into(),
                },
                ContentPart::Image {
                    url: "data:image/png;base64,iVBORw0KGgo=".into(),
                },
            ],
            synthetic_reason: None,
            ..Default::default()
        }),
        ConversationItem::assistant("I see it"),
        ConversationItem::user("follow up question"),
    ]);

    let req = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();

    // The old user turn's image must survive (small payload, far under 50 MB),
    // so the KV-cache prefix stays byte-stable instead of being rewritten.
    let image_retained = req.items.iter().any(|item| {
        matches!(item, ConversationItem::User(u)
            if u.content.iter().any(|p| matches!(p, ContentPart::Image { .. })))
    });
    assert!(
        image_retained,
        "a small old image must be preserved, not stripped to a placeholder"
    );
}

#[tokio::test]
async fn build_request_budgets_tool_images_on_request_copy_only() {
    use echo_agent_sampling_types::{ContentPart, ToolCall};

    let image_url = format!(
        "data:image/png;base64,{}",
        "A".repeat(crate::image_budget::IMAGE_COMPACT_TRIGGER_BYTES)
    );
    let source = vec![
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: r#"{"target_file":"image.png"}"#.into(),
        }]),
        ConversationItem::tool_result_with_images(
            "call-1",
            "tool text",
            vec![ContentPart::Image {
                url: image_url.into(),
            }],
        ),
    ];
    let source_bytes = serde_json::to_vec(&source).unwrap().len();
    let mut h = TestHarness::with_conversation(source);
    let request = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r".into())
        .await
        .unwrap();
    let event = h.next_event().await;
    let canonical = h.handle.get_conversation().await;

    let ChatStateEvent::ImageBudget {
        body_bytes,
        body_bytes_after,
        inline_images,
        needs_image_compaction,
        evicted,
        ..
    } = event
    else {
        panic!("expected image budget event");
    };
    assert_eq!(body_bytes, source_bytes);
    assert_eq!(
        body_bytes_after,
        serde_json::to_vec(&request.items).unwrap().len()
    );
    assert_eq!(inline_images, 1);
    assert!(needs_image_compaction);
    assert_eq!(evicted, 1);
    let ConversationItem::ToolResult(request_result) = &request.items[1] else {
        panic!("expected request tool result");
    };
    assert!(request_result.images.is_empty());
    assert_eq!(request_result.tool_call_id, "call-1");
    assert!(request_result.content.starts_with("tool text\n\n"));
    assert!(
        request_result
            .content
            .contains("images from this tool result were removed")
    );
    let ConversationItem::ToolResult(canonical_result) = &canonical[1] else {
        panic!("expected canonical tool result");
    };
    assert_eq!(canonical_result.images.len(), 1);
    assert_eq!(canonical_result.content.as_ref(), "tool text");
}

/// Prefix stability after tool result pruning. When context utilization
/// exceeds 50%, old tool results are soft-trimmed or hard-cleared, but
/// this happens on a clone -- items outside the pruned region must
/// remain identical.
#[tokio::test]
async fn prefix_stable_after_tool_result_pruning() {
    let h = TestHarness::with_context_window(10_000);

    h.handle.push_user_message(ConversationItem::system("sys"));
    h.handle.push_user_message(ConversationItem::user("q1"));
    h.handle
        .push_assistant_response(ConversationItem::assistant("a1"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("c1", "x".repeat(500)));
    h.handle.push_user_message(ConversationItem::user("q2"));

    let req1 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r1".into())
        .await
        .unwrap();

    h.handle
        .push_assistant_response(ConversationItem::assistant("a2"));
    h.handle
        .push_tool_result(ConversationItem::tool_result("c2", "y".repeat(500)));
    h.handle.push_user_message(ConversationItem::user("q3"));
    h.handle.record_token_usage(6000); // > 50% of 10k context

    let req2 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r2".into())
        .await
        .unwrap();

    let body1 = serialize_via_public_api(&req1);
    let body2 = serialize_via_public_api(&req2);
    let input1 = body1["input"].as_array().unwrap();
    let input2 = body2["input"].as_array().unwrap();

    assert_eq!(
        input1[0], input2[0],
        "system prompt must be stable after pruning"
    );
    assert!(
        input2.len() >= input1.len(),
        "pruned request should still have >= items"
    );

    let extract_user_texts = |input: &[serde_json::Value]| -> Vec<String> {
        input
            .iter()
            .filter_map(|v| {
                if v.get("role").and_then(|r| r.as_str()) == Some("user") {
                    v.get("content").and_then(|c| c.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect()
    };
    let users1 = extract_user_texts(input1);
    let users2 = extract_user_texts(input2);
    let mut idx2 = 0;
    for u1 in &users1 {
        while idx2 < users2.len() && &users2[idx2] != u1 {
            idx2 += 1;
        }
        assert!(
            idx2 < users2.len(),
            "user message {u1:?} from req1 must appear in req2 in same order"
        );
        idx2 += 1;
    }
}

/// `BackendToolCall` items (web search) must serialize stably across
/// turns. Once emitted, a backend tool call sits at a fixed position
/// in the conversation list and the wire input.
#[tokio::test]
async fn prefix_stable_with_backend_tool_calls() {
    use echo_agent_sampling_types::{BackendToolCallItem, BackendToolKind, rs};

    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("search for capybaras"),
    ]);

    let req1 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r1".into())
        .await
        .unwrap();

    h.handle
        .push_assistant_response(ConversationItem::assistant("Found info about capybaras"));
    h.handle
        .push_tool_result(ConversationItem::BackendToolCall(BackendToolCallItem {
            kind: BackendToolKind::WebSearch(rs::WebSearchToolCall {
                id: "ws_capybara".to_string(),
                status: rs::WebSearchToolCallStatus::Completed,
                action: rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                    query: "capybara facts".to_string(),
                    sources: Some(vec![]),
                }),
            }),
        }));
    h.handle
        .push_user_message(ConversationItem::user("tell me more"));

    let req2 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r2".into())
        .await
        .unwrap();

    assert_prefix_stable_pair(&req1, &req2, "with backend tool calls");

    h.handle
        .push_assistant_response(ConversationItem::assistant("More capybara info"));
    h.handle.push_user_message(ConversationItem::user("thanks"));

    let req3 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r3".into())
        .await
        .unwrap();

    assert_prefix_stable_pair(&req2, &req3, "backend tool call stable across 3 turns");
}

/// Snapshot/restore must preserve prefix stability within the same
/// compaction epoch. After restoring a snapshot, build_request output
/// should match what it was before the restore.
#[tokio::test]
async fn prefix_stable_after_session_resume() {
    let h = TestHarness::with_conversation(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hello"),
    ]);

    let req1 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r1".into())
        .await
        .unwrap();

    h.handle
        .push_assistant_response(ConversationItem::assistant("hi"));
    h.handle.push_user_message(ConversationItem::user("q2"));

    let req2 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r2".into())
        .await
        .unwrap();

    let snapshot = h.handle.snapshot().await.unwrap();

    h.handle
        .replace_conversation(vec![ConversationItem::system("different")]);

    h.handle.restore_snapshot(snapshot);

    let req3 = h
        .handle
        .build_request(vec![], None, false, None, "c".into(), "r3".into())
        .await
        .unwrap();

    assert_prefix_stable_pair(&req1, &req3, "after session resume");

    let body2 = serialize_via_public_api(&req2);
    let body3 = serialize_via_public_api(&req3);
    let input2 = body2["input"].as_array().unwrap();
    let input3 = body3["input"].as_array().unwrap();
    assert_eq!(
        input2, input3,
        "restored snapshot must produce identical request items"
    );
}

// ============================================================================
// Out-of-band history repair (echo.agent/session/repair)
// ============================================================================

/// Bricked-session shape: an orphaned tool result survives load (the eager
/// repairs only fix dangling calls) and 400s on every request. The
/// out-of-band `RepairHistory` command must strip it and persist the fix.
#[tokio::test]
async fn repair_history_command_strips_orphan_and_persists() {
    use echo_agent_sampling_types::ToolCall;

    let corrupted = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("prompt"),
        // ← assistant owning call_LOST is missing (skipped corrupt line)
        ConversationItem::tool_result("call_LOST", "orphaned"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_OK".into(),
            name: "read_file".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("call_OK", "fine"),
    ];
    let mut h = TestHarness::with_conversation(corrupted);

    // Load-time repairs leave the orphan in place (the bug under test).
    assert_eq!(h.handle.get_conversation().await.len(), 5);
    h.drain_persistence();

    // Dry run: reports the orphan, mutates nothing, persists nothing.
    let report = h.handle.repair_history(true, None).await.unwrap().unwrap();
    assert!(report.changed());
    assert_eq!(report.stripped_tool_result_ids, vec!["call_LOST"]);
    assert_eq!(h.handle.get_conversation().await.len(), 5);
    assert!(h.drain_persistence().is_empty(), "dry run must not persist");

    // Real repair: orphan stripped, fix persisted via a full history replace.
    let report = h.handle.repair_history(false, None).await.unwrap().unwrap();
    assert!(report.changed());
    assert_eq!(report.stripped_tool_result_ids, vec!["call_LOST"]);

    let conv = h.handle.get_conversation().await;
    assert_eq!(conv.len(), 4);
    assert!(!conv.iter().any(|i| matches!(
        i,
        ConversationItem::ToolResult(tr) if tr.tool_call_id == "call_LOST"
    )));

    let replaced = h
        .drain_persistence()
        .into_iter()
        .find_map(|r| match r {
            PersistenceRecord::ReplaceHistory(items) => Some(items),
            _ => None,
        })
        .expect("repair must persist a full history replace");
    assert_eq!(replaced.len(), 4);

    // Idempotent: a second repair reports no changes and persists nothing.
    let report = h.handle.repair_history(false, None).await.unwrap().unwrap();
    assert!(!report.changed());
    assert!(h.drain_persistence().is_empty());
}

/// A repair command processed while the shared turn-active flag is set must
/// be refused without mutating or persisting anything (the in-actor check
/// closes the race with a turn starting after the caller's own check).
#[tokio::test]
async fn repair_history_command_refused_while_turn_active() {
    let corrupted = vec![
        ConversationItem::user("prompt"),
        ConversationItem::tool_result("call_ORPHAN", "orphaned"),
    ];
    let mut h = TestHarness::with_conversation(corrupted);
    h.drain_persistence();

    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let result = h
        .handle
        .repair_history(false, Some(flag.clone()))
        .await
        .unwrap();
    assert!(matches!(result, Err(crate::commands::RepairHistoryBlocked)));
    // Nothing was mutated or persisted.
    assert_eq!(h.handle.get_conversation().await.len(), 2);
    assert!(h.drain_persistence().is_empty());

    // Once the turn ends, the same call succeeds.
    flag.store(false, std::sync::atomic::Ordering::SeqCst);
    let report = h
        .handle
        .repair_history(false, Some(flag))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.stripped_tool_result_ids, vec!["call_ORPHAN"]);
}
