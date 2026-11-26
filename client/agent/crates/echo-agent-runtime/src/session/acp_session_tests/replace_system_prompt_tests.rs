//! Actor-path coverage for `handle_replace_system_prompt` — the
//! resident-reconnect `systemPromptOverride` sync. Head-swap semantics are
//! unit-tested in `echo_agent_chat_state` (`conversation_util` and the actor tests);
//! these cover only what is unique to the `SessionActor` seam: the end-to-end
//! swap and the `preserve_inherited_system` skip.

use echo_agent_sampling_types::conversation::ConversationItem;

use super::support::create_test_actor;
use super::{PersistenceMsg, SessionActor};

fn head_text(conv: &[ConversationItem]) -> Option<String> {
    match conv.first() {
        Some(ConversationItem::System(sys)) => Some(sys.content.to_string()),
        _ => None,
    }
}

async fn actor_with_history(history: Vec<ConversationItem>) -> SessionActor {
    let (gateway_tx, _grx) =
        tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
    let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
    let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    actor.chat_state_handle.replace_conversation(history);
    actor
}

#[tokio::test(flavor = "current_thread")]
async fn handle_replace_system_prompt_replaces_head_and_preserves_turns() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = actor_with_history(vec![
                ConversationItem::system("composer default"),
                ConversationItem::user("hi"),
                ConversationItem::assistant("yo"),
            ])
            .await;

            actor
                .handle_replace_system_prompt("client override".to_string())
                .await;

            let conv = actor.chat_state_handle.get_conversation().await;
            assert_eq!(head_text(&conv).as_deref(), Some("client override"));
            assert_eq!(conv.len(), 3, "must not wipe user/assistant turns");
            assert!(matches!(conv[1], ConversationItem::User(_)));
            assert!(matches!(conv[2], ConversationItem::Assistant(_)));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn handle_replace_system_prompt_skips_on_preserve_inherited_system() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut actor = actor_with_history(vec![
                ConversationItem::system("parent verbatim"),
                ConversationItem::user("hi"),
            ])
            .await;
            // Verbatim mirror-fork: the inherited cache prefix must survive.
            actor.startup_hints.preserve_inherited_system = true;

            actor
                .handle_replace_system_prompt("client override".to_string())
                .await;

            let conv = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                head_text(&conv).as_deref(),
                Some("parent verbatim"),
                "preserve_inherited_system must not overwrite the inherited head"
            );
        })
        .await;
}
