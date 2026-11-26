//! Media-gen per-batch cap: Pending ToolCall + Failed update + tool_result pairing.
//!
//! Regression for the reject path that previously shipped orphan ToolCallUpdates
//! and broke streaming-messages-json reducers (GBT-5790 council rounds).
//!
//! `send_update` enqueues on the actor's `event_tx` (SessionEvent::Notification);
//! the session run loop fans those out to the gateway. Actor-level tests assert
//! on `event_rx`, not the gateway channel.

use super::support::*;
use super::*;
use agent_client_protocol as acp;
use echo_agent_tools::implementations::echo_agent_build::image_gen::ImageGenTool;
use echo_agent_tools::media_gen_limits::DEFAULT_MAX_PARALLEL_IMAGE_GEN;
use echo_agent_tools::registry::types::ToolConfig;

fn image_gen_call(id: &str) -> ToolCallResponse {
    ToolCallResponse {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::sampling::types::ToolCallFunction::new(
            "image_gen",
            r#"{"prompt":"media-gen-batch-limit test"}"#,
        ),
    }
}

fn read_file_call(id: &str) -> ToolCallResponse {
    ToolCallResponse {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::sampling::types::ToolCallFunction::new(
            "read_file",
            r#"{"target_file":"/tmp/media-gen-batch-limit-sibling.txt"}"#,
        ),
    }
}

fn drain_tool_call_statuses(
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
) -> Vec<(String, acp::ToolCallStatus)> {
    let mut statuses = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        let SessionEvent::Notification(notification) = event else {
            continue;
        };
        let Some(acp_n) = (match notification {
            SessionNotification::Acp(n) => Some(*n),
            SessionNotification::EchoAgent(_) => None,
        }) else {
            continue;
        };
        match acp_n.update {
            acp::SessionUpdate::ToolCall(tc) => {
                statuses.push((tc.tool_call_id.0.as_ref().to_string(), tc.status));
            }
            acp::SessionUpdate::ToolCallUpdate(upd) => {
                if let Some(status) = upd.fields.status {
                    statuses.push((upd.tool_call_id.0.as_ref().to_string(), status));
                }
            }
            _ => {}
        }
    }
    statuses
}

async fn tool_result_text(actor: &SessionActor, call_id: &str) -> String {
    let conv = actor.chat_state_handle.get_conversation().await;
    conv.iter()
        .rev()
        .find_map(|item| match item {
            echo_agent_sampling_types::ConversationItem::ToolResult(tr)
                if tr.tool_call_id == call_id =>
            {
                Some(tr.content.to_string())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no tool_result for {call_id} in {conv:?}"))
}

#[tokio::test(flavor = "current_thread")]
async fn first_k_tail_rejects_get_pending_then_failed() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let (mut actor, mut event_rx) =
                create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
            // max_image=0 → first-K admits none, so ImageGen never runs.
            std::sync::Arc::get_mut(&mut actor.rebuild_spec)
                .expect("test rebuild_spec is uniquely owned")
                .media_gen_batch_limits
                .max_image = 0;
            *actor.agent.borrow_mut() = test_agent_with_tools(vec![
                ToolConfig::for_tool::<ImageGenTool>(),
                ToolConfig::from_id("EchoAgentBuild:read_file"),
            ])
            .await;

            let mut batch: Vec<ToolCallResponse> = (0..3)
                .map(|i| image_gen_call(&format!("img_{i}")))
                .collect();
            batch.push(read_file_call("read_sibling"));

            tokio::time::timeout(
                std::time::Duration::from_secs(15),
                actor.execute_tool_calls(batch),
            )
            .await
            .expect("execute_tool_calls must not hang")
            .expect("execute_tool_calls must not error");

            for i in 0..3 {
                let id = format!("img_{i}");
                let text = tool_result_text(&actor, &id).await;
                assert!(
                    text.contains("at most")
                        && text.contains("image_gen")
                        && text.contains("This extra call was skipped"),
                    "reject text for {id}: {text}"
                );
            }

            let sibling = tool_result_text(&actor, "read_sibling").await;
            assert!(
                !sibling.contains("This extra call was skipped"),
                "sibling must not get the media-gen reject message: {sibling}"
            );

            let statuses = drain_tool_call_statuses(&mut event_rx);
            for i in 0..3 {
                let id = format!("img_{i}");
                let for_id: Vec<_> = statuses
                    .iter()
                    .filter(|(cid, _)| cid == &id)
                    .map(|(_, s)| *s)
                    .collect();
                assert!(
                    for_id.len() >= 2,
                    "id {id} needs Pending then Failed, got {for_id:?} from {statuses:?}"
                );
                assert_eq!(
                    for_id[0],
                    acp::ToolCallStatus::Pending,
                    "id {id} first status must be Pending: {for_id:?}"
                );
                assert!(
                    for_id.contains(&acp::ToolCallStatus::Failed),
                    "id {id} must reach Failed: {for_id:?}"
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn over_cap_report_classifies_modest_vs_egregious() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_tools(vec![
                ToolConfig::for_tool::<ImageGenTool>(),
                ToolConfig::from_id("EchoAgentBuild:read_file"),
            ])
            .await;

            let over = DEFAULT_MAX_PARALLEL_IMAGE_GEN + 1;
            let calls: Vec<echo_agent_sampling_types::ToolCall> = (0..over)
                .map(|i| echo_agent_sampling_types::ToolCall {
                    id: format!("img_{i}").into(),
                    name: "image_gen".into(),
                    arguments: "{}".into(),
                })
                .collect();
            let report = actor.media_gen_over_cap(&calls);
            assert_eq!(report.len(), 1);
            assert_eq!(report[0].name, "image_gen");
            assert_eq!(report[0].total, over);
            assert_eq!(report[0].max, DEFAULT_MAX_PARALLEL_IMAGE_GEN);
            assert!(
                !report[0].is_egregious(),
                "max+1 is modest first-K, not a 2x resample"
            );

            let spam: Vec<echo_agent_sampling_types::ToolCall> = (0
                ..DEFAULT_MAX_PARALLEL_IMAGE_GEN * 2)
                .map(|i| echo_agent_sampling_types::ToolCall {
                    id: format!("spam_{i}").into(),
                    name: "image_gen".into(),
                    arguments: "{}".into(),
                })
                .collect();
            let spam_report = actor.media_gen_over_cap(&spam);
            assert_eq!(spam_report.len(), 1);
            assert!(spam_report[0].is_egregious());

            let under: Vec<echo_agent_sampling_types::ToolCall> = (0
                ..DEFAULT_MAX_PARALLEL_IMAGE_GEN)
                .map(|i| echo_agent_sampling_types::ToolCall {
                    id: format!("ok_{i}").into(),
                    name: "image_gen".into(),
                    arguments: "{}".into(),
                })
                .collect();
            assert!(actor.media_gen_over_cap(&under).is_empty());
        })
        .await;
}
