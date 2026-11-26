//! Image-strip persistence policy (`acp_session_impl/image_strip.rs`):
//! which `ImagesStripped` events may rewrite stored history, the deferred
//! persist that waits for the stripped retry's `Completed`, and the user
//! notifications for both the request-local and the durable case.

use std::sync::Arc;

use super::support::*;
use super::*;
use echo_agent_sampler::{InferenceLatencyStats, RequestId, SamplingEvent, StripReason};
use echo_agent_sampling_types::{ContentPart, ConversationItem, ConversationResponse};

const PERSIST_GATE_IMAGE_URI: &str = "data:image/png;base64,KEEPME";

fn user_with_image(url: &str) -> ConversationItem {
    let mut user = match ConversationItem::user("look at this") {
        ConversationItem::User(u) => u,
        _ => unreachable!(),
    };
    user.add_image(url);
    ConversationItem::User(user)
}

fn conversation_has_image(conv: &[ConversationItem], url: &str) -> bool {
    conv.iter().any(|item| match item {
        ConversationItem::User(u) => u
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { url: u } if u.as_ref() == url)),
        _ => false,
    })
}

async fn seed_image(actor: &SessionActor, url: &str) {
    actor
        .chat_state_handle
        .push_user_message(user_with_image(url));
    let conv = actor.chat_state_handle.get_conversation().await;
    assert!(
        conversation_has_image(&conv, url),
        "precondition: seeded image must be in chat-state"
    );
}

/// Drain the gateway channel into debug strings for notification assertions.
fn drain_gateway_debug(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
) -> String {
    let mut out = String::new();
    while let Ok(msg) = rx.try_recv() {
        out.push_str(&format!("{msg:?}\n"));
    }
    out
}

/// The deferred apply runs as a detached local task with nothing to join,
/// and several callers assert absence afterwards; that needs a window, not
/// a completion signal. Yield to the LocalSet for a wall-clock bound.
async fn settle() {
    let _ = tokio::time::timeout(std::time::Duration::from_millis(100), async {
        loop {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

/// Wait until the stored conversation satisfies `cond`, bounded by wall
/// clock; on timeout returns the last-read conversation so the caller's
/// assertion fails showing the real state.
async fn wait_for_conversation(
    actor: &SessionActor,
    cond: impl Fn(&[ConversationItem]) -> bool,
) -> Vec<ConversationItem> {
    let poll = async {
        loop {
            let conv = actor.chat_state_handle.get_conversation().await;
            if cond(&conv) {
                return conv;
            }
            tokio::task::yield_now().await;
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(5), poll).await {
        Ok(conv) => conv,
        Err(_) => actor.chat_state_handle.get_conversation().await,
    }
}

fn completed_event(request_id: &RequestId) -> SamplingEvent {
    SamplingEvent::Completed {
        request_id: request_id.clone(),
        response: Box::new(ConversationResponse {
            items: vec![ConversationItem::assistant("recovered")],
            stop_reason: None,
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 1,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        }),
        metrics: InferenceLatencyStats::default(),
    }
}

fn failed_info() -> echo_agent_sampler::SamplingErrorInfo {
    echo_agent_sampler::SamplingErrorInfo {
        kind: echo_agent_sampler::SamplingErrorKind::Api,
        message: "400 Bad Request".to_string(),
        status_code: Some(400),
        is_retryable: false,
        retry_after_secs: None,
        should_retry: None,
        error_code: None,
        model_metadata: None,
        empty_response_context: None,
        doom_loop_triggers: None,
        doom_loop_aborted_at_chunk: None,
        credential: echo_agent_sampling_types::SentCredential::Unknown,
    }
}

fn images_stripped(request_id: &RequestId, urls: &[&str], reason: StripReason) -> SamplingEvent {
    SamplingEvent::ImagesStripped {
        request_id: request_id.clone(),
        stripped_urls: urls.iter().map(|u| Arc::<str>::from(*u)).collect(),
        reason,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn heuristic_images_stripped_does_not_rewrite_history() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor =
                Arc::new(create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await);
            seed_image(&actor, PERSIST_GATE_IMAGE_URI).await;
            let rid = RequestId::from("req-heuristic");

            actor
                .handle_sampling_event(images_stripped(
                    &rid,
                    &[PERSIST_GATE_IMAGE_URI],
                    StripReason::PayloadHeuristic,
                ))
                .await;
            actor.handle_sampling_event(completed_event(&rid)).await;
            settle().await;

            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                conversation_has_image(&conv, PERSIST_GATE_IMAGE_URI),
                "PayloadHeuristic must stay request-local even after Completed: {conv:?}"
            );
            let sent = drain_gateway_debug(&mut gateway_rx);
            assert!(
                sent.contains("left out of the retry"),
                "request-local strip must tell the user with the retry wording, sent: {sent}"
            );
        })
        .await;
}

/// The durable path: a server-confirmed single-image strip is buffered on
/// `ImagesStripped` (history untouched), persisted when the stripped
/// retry's `Completed` proves it helped, and the user is told only then.
#[tokio::test(flavor = "current_thread")]
async fn server_rejected_strip_persists_only_after_completed() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor =
                Arc::new(create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await);
            seed_image(&actor, PERSIST_GATE_IMAGE_URI).await;
            let rid = RequestId::from("req-rejected");

            actor
                .handle_sampling_event(images_stripped(
                    &rid,
                    &[PERSIST_GATE_IMAGE_URI],
                    StripReason::ServerRejected,
                ))
                .await;
            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                conversation_has_image(&conv, PERSIST_GATE_IMAGE_URI),
                "deletion must wait for the stripped retry to succeed: {conv:?}"
            );
            assert!(
                !drain_gateway_debug(&mut gateway_rx).contains("removed from the conversation"),
                "no durable-removal note before the retry succeeds"
            );

            // A Completed for a DIFFERENT request must not consume the buffer.
            actor
                .handle_sampling_event(completed_event(&RequestId::from("req-unrelated")))
                .await;
            settle().await;
            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                conversation_has_image(&conv, PERSIST_GATE_IMAGE_URI),
                "a mismatched request id must leave the buffer intact: {conv:?}"
            );

            actor.handle_sampling_event(completed_event(&rid)).await;
            let conv = wait_for_conversation(&actor, |conv| {
                !conversation_has_image(conv, PERSIST_GATE_IMAGE_URI)
            })
            .await;
            assert!(
                !conversation_has_image(&conv, PERSIST_GATE_IMAGE_URI),
                "Completed must apply the buffered strip: {conv:?}"
            );
            settle().await; // the note follows the disk ack
            let sent = drain_gateway_debug(&mut gateway_rx);
            assert!(
                sent.contains("removed from the conversation"),
                "persisted strip must tell the user it is permanent, sent: {sent}"
            );
        })
        .await;
}

/// A strip that does not reach `Applied` must still tell the user the
/// answer was produced without the image; it just must not claim the
/// stored conversation changed.
#[tokio::test(flavor = "current_thread")]
async fn non_applied_strip_outcome_still_notifies_the_user() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor =
                Arc::new(create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await);
            // Nothing seeded: the buffered URL matches no stored image, so
            // the apply resolves as `NoMatch` rather than `Applied`.
            let rid = RequestId::from("req-no-match");

            actor
                .handle_sampling_event(images_stripped(
                    &rid,
                    &[PERSIST_GATE_IMAGE_URI],
                    StripReason::ServerRejected,
                ))
                .await;
            actor.handle_sampling_event(completed_event(&rid)).await;
            settle().await;

            let sent = drain_gateway_debug(&mut gateway_rx);
            assert!(
                sent.contains("left out of"),
                "a non-Applied outcome must still tell the user, sent: {sent}"
            );
            assert!(
                !sent.contains("removed from the conversation"),
                "only Applied may claim the stored conversation changed, sent: {sent}"
            );
        })
        .await;
}

/// A strip that did not rescue the turn proves nothing: `Failed` drops the
/// buffer and stored history keeps its images.
#[tokio::test(flavor = "current_thread")]
async fn server_rejected_strip_dropped_when_retry_fails() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor =
                Arc::new(create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await);
            seed_image(&actor, PERSIST_GATE_IMAGE_URI).await;
            let rid = RequestId::from("req-rejected-then-fatal");

            actor
                .handle_sampling_event(images_stripped(
                    &rid,
                    &[PERSIST_GATE_IMAGE_URI],
                    StripReason::ServerRejected,
                ))
                .await;
            // The drop must be wired through the event handler itself,
            // deleting the Failed arm's call must fail this test.
            actor
                .handle_sampling_event(SamplingEvent::Failed {
                    request_id: rid.clone(),
                    error: failed_info(),
                })
                .await;
            // A later Completed for the same id must be a no-op.
            actor.handle_sampling_event(completed_event(&rid)).await;
            settle().await;

            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                conversation_has_image(&conv, PERSIST_GATE_IMAGE_URI),
                "a dropped strip must never persist: {conv:?}"
            );
        })
        .await;
}

/// Blame is judged on unique URLs: two DISTINCT stripped images are
/// ambiguous and stay request-local; the same image stored twice is one
/// suspect and persists (both occurrences).
#[tokio::test(flavor = "current_thread")]
async fn multi_image_blame_is_judged_on_unique_urls() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor =
                Arc::new(create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await);
            let second_uri = "data:image/png;base64,c2Vjb25kLWltYWdl";
            seed_image(&actor, PERSIST_GATE_IMAGE_URI).await;
            seed_image(&actor, second_uri).await;

            // Two distinct URLs: ambiguous, never persists.
            let rid = RequestId::from("req-ambiguous");
            actor
                .handle_sampling_event(images_stripped(
                    &rid,
                    &[PERSIST_GATE_IMAGE_URI, second_uri],
                    StripReason::ServerRejected,
                ))
                .await;
            actor.handle_sampling_event(completed_event(&rid)).await;
            settle().await;
            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                conversation_has_image(&conv, PERSIST_GATE_IMAGE_URI)
                    && conversation_has_image(&conv, second_uri),
                "ambiguous blame must not delete stored images: {conv:?}"
            );

            // The same URL twice (attached in two turns): one suspect,
            // persists, removing both stored occurrences.
            seed_image(&actor, PERSIST_GATE_IMAGE_URI).await;
            let rid = RequestId::from("req-duplicate");
            actor
                .handle_sampling_event(images_stripped(
                    &rid,
                    &[PERSIST_GATE_IMAGE_URI, PERSIST_GATE_IMAGE_URI],
                    StripReason::ServerRejected,
                ))
                .await;
            actor.handle_sampling_event(completed_event(&rid)).await;
            let conv = wait_for_conversation(&actor, |conv| {
                !conversation_has_image(conv, PERSIST_GATE_IMAGE_URI)
            })
            .await;
            assert!(
                !conversation_has_image(&conv, PERSIST_GATE_IMAGE_URI),
                "a single unique URL is unambiguous blame: {conv:?}"
            );
            assert!(
                conversation_has_image(&conv, second_uri),
                "the unrelated image must survive: {conv:?}"
            );
        })
        .await;
}
