//! Mid-turn interjection tests: formatting, broadcast, and the drain path.
use super::support::*;
use super::*;

/// Draining a mid-turn interjection pushes a standalone synthetic user
/// message tagged [`SyntheticReason::Interjection`] — even when the
/// conversation tail is a `ToolResult`. The tool result content must be
/// left untouched (interjections are never appended to tool results).
#[tokio::test]
async fn drain_interjections_pushes_synthetic_user_message_after_tool_result() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;

            const TOOL_RESULT_CONTENT: &str = "file contents: fn main() {}";
            actor
                .chat_state_handle
                .push_tool_result(ConversationItem::tool_result("call-1", TOOL_RESULT_CONTENT));
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "please also add tests".to_string(),
                attachments: vec![],
            });

            assert!(
                actor.drain_pending_interjections().await,
                "drain must report that an interjection was consumed"
            );
            assert!(
                actor.pending_interjections.is_empty(),
                "buffer must be empty after drain"
            );

            let conversation = actor.chat_state_handle.get_conversation().await;

            // The tool result is untouched — no interjection text bundled in.
            let tool_result = conversation
                .iter()
                .find_map(|item| match item {
                    ConversationItem::ToolResult(tr) => Some(tr),
                    _ => None,
                })
                .expect("seeded tool result must still be in the conversation");
            assert_eq!(
                tool_result.content.as_ref(),
                TOOL_RESULT_CONTENT,
                "tool result content must not be mutated by an interjection"
            );

            // The interjection landed as a standalone synthetic user message
            // after the tool result.
            let user_item = match conversation.last() {
                Some(ConversationItem::User(u)) => u,
                other => panic!("conversation tail must be a user item, got: {other:?}"),
            };
            assert_eq!(
                user_item.synthetic_reason,
                Some(SyntheticReason::Interjection),
                "interjection must be tagged SyntheticReason::Interjection"
            );
            let text = conversation
                .last()
                .expect("non-empty conversation")
                .text_content();
            assert!(
                text.contains("<user_query>") && text.contains("please also add tests"),
                "interjection must carry the wrapped user text, got: {text}"
            );
        })
        .await;
}

/// Multiple buffered interjections drain as one standalone synthetic user
/// message EACH, in FIFO order (Ctrl+Enter twice = two tagged user rows).
/// None of them may touch the tool result at the conversation tail.
#[tokio::test]
async fn drain_multiple_interjections_pushes_one_user_message_each_in_order() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;

            const TOOL_RESULT_CONTENT: &str = "tool output";
            actor
                .chat_state_handle
                .push_tool_result(ConversationItem::tool_result("call-1", TOOL_RESULT_CONTENT));
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "first steer".to_string(),
                attachments: vec![],
            });
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "second steer".to_string(),
                attachments: vec![],
            });
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "third steer".to_string(),
                attachments: vec![],
            });

            assert!(actor.drain_pending_interjections().await);
            assert!(actor.pending_interjections.is_empty());

            let conversation = actor.chat_state_handle.get_conversation().await;

            let tool_result = conversation
                .iter()
                .find_map(|item| match item {
                    ConversationItem::ToolResult(tr) => Some(tr),
                    _ => None,
                })
                .expect("seeded tool result must still be in the conversation");
            assert_eq!(
                tool_result.content.as_ref(),
                TOOL_RESULT_CONTENT,
                "tool result must not absorb any of the interjections"
            );

            // Exactly one tagged user row per interjection, in send order.
            let ij_texts: Vec<String> = conversation
                .iter()
                .filter_map(|item| match item {
                    ConversationItem::User(u)
                        if u.synthetic_reason == Some(SyntheticReason::Interjection) =>
                    {
                        Some(item.text_content())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                ij_texts.len(),
                3,
                "each interjection must land as its own user row, got: {ij_texts:?}"
            );
            for (text, expected) in
                ij_texts
                    .iter()
                    .zip(["first steer", "second steer", "third steer"])
            {
                assert!(
                    text.contains(expected) && text.contains("<user_query>"),
                    "interjection rows must keep FIFO order; expected {expected:?} in {text:?}"
                );
            }
        })
        .await;
}

/// Draining with an empty buffer reports false and leaves the conversation
/// untouched. The turn loop's checkpoint gates rely on this.
#[tokio::test]
async fn drain_with_empty_buffer_is_a_noop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            let before = actor.chat_state_handle.get_conversation().await.len();
            assert!(!actor.drain_pending_interjections().await);
            let after = actor.chat_state_handle.get_conversation().await.len();
            assert_eq!(before, after, "empty drain must not touch the conversation");
        })
        .await;
}

mod interjection_format_tests {
    use super::format_interjection;

    #[test]
    fn interjection_wraps_text_in_user_query() {
        let wrapped = format_interjection("please also add tests".to_string());
        assert!(
            wrapped.contains("<user_query>\nplease also add tests\n</user_query>"),
            "interjection should wrap the user's message in <user_query> tags, got: {wrapped}"
        );
    }

    #[test]
    fn interjection_reminds_to_finish_previous_work() {
        let wrapped = format_interjection("please also add tests".to_string());
        assert!(
            !wrapped.contains("After completing your current task"),
            "interjection must not defer the user's message, got: {wrapped}"
        );
        assert!(
            wrapped.trim_end().ends_with(
                "</user_query>\nMake sure to complete any unfinished tasks from previous turns."
            ),
            "unfinished-task reminder must follow the wrapped query, got: {wrapped}"
        );
    }
}

mod interjection_broadcast_tests {
    use super::support::create_test_actor;
    use super::*;

    /// Multi-client fix: a mid-turn interjection must be broadcast to every
    /// attached client (not just the originator) so all panes viewing the same
    /// session render it. This locks the wire contract the pager's
    /// `handle_interjection` depends on: method `echo.agent/session/interjection`
    /// carrying `sessionId` + `text`.
    #[tokio::test]
    async fn broadcast_interjection_emits_sessionid_and_text() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (gateway_tx, mut gateway_rx) =
                    tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
                let (persistence_tx, _prx) =
                    tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
                let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

                actor.broadcast_interjection("please also add tests", Some("ij-1"));

                let mut payload = None;
                while let Ok(msg) = gateway_rx.try_recv() {
                    if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
                        && args.request.method.as_ref() == "echo.agent/session/interjection"
                    {
                        payload =
                            serde_json::from_str::<serde_json::Value>(args.request.params.get())
                                .ok();
                    }
                }
                let payload = payload.expect("an echo.agent/session/interjection broadcast");
                assert_eq!(
                    payload.get("sessionId").and_then(|v| v.as_str()),
                    Some("test-actor"),
                    "broadcast must carry the session id"
                );
                assert_eq!(
                    payload.get("text").and_then(|v| v.as_str()),
                    Some("please also add tests"),
                    "broadcast must carry the interjection text verbatim"
                );
                assert_eq!(
                    payload.get("interjectionId").and_then(|v| v.as_str()),
                    Some("ij-1"),
                    "broadcast must echo the interjection id for originator dedup"
                );
            })
            .await;
    }
}
