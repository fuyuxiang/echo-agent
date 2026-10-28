//! Shell-specific memory flush conversation window selection.

use echo_agent_sampling_types::ConversationItem;

pub const MAX_FLUSH_MESSAGES: usize = 80;
pub const MAX_FLUSH_BYTES: usize = 256 * 1024;
const MAX_ITEM_BYTES: usize = 32 * 1024;

/// Select a recent typed-history window for the flush model.
///
/// Starts with the last `recent_message_count` messages, then expands backward
/// to the nearest `User` message so the window always starts on a user
/// boundary when it fits. Long single-user turns retain the user anchor and
/// recent outcomes within hard message and serialized-byte budgets.
/// System messages are excluded since the flush adds its own system prompt.
pub fn select_flush_window(
    messages: Vec<ConversationItem>,
    recent_message_count: usize,
) -> Vec<ConversationItem> {
    // The memory model needs conversation evidence, not signed reasoning,
    // images or executable tool-call pairs that a bounded window may split.
    let messages: Vec<_> =
        echo_agent_chat_state::compaction_utils::prepare_conversation_for_summarization(messages)
            .into_iter()
            .filter(|item| !matches!(item, ConversationItem::System(_)))
            .collect();

    if messages.is_empty() || recent_message_count == 0 {
        return Vec::new();
    }
    let total = messages.len();
    let mut start = total.saturating_sub(recent_message_count);
    while start > 0 && !matches!(messages[start], ConversationItem::User(_)) {
        start -= 1;
    }
    let mut window = messages.into_iter().skip(start);
    let anchor = window.next().and_then(bounded_item);
    let mut bytes = anchor.as_ref().map_or(0, serialized_size);
    let mut tail = Vec::new();
    for item in window.collect::<Vec<_>>().into_iter().rev() {
        if tail.len() + usize::from(anchor.is_some()) >= MAX_FLUSH_MESSAGES {
            break;
        }
        let Some(item) = bounded_item(item) else {
            continue;
        };
        let size = serialized_size(&item);
        if bytes + size <= MAX_FLUSH_BYTES {
            bytes += size;
            tail.push(item);
        }
    }
    tail.reverse();
    anchor.into_iter().chain(tail).collect()
}

fn serialized_size(item: &ConversationItem) -> usize {
    serde_json::to_vec(item).map_or(usize::MAX, |bytes| bytes.len())
}

fn bounded_item(mut item: ConversationItem) -> Option<ConversationItem> {
    if serialized_size(&item) <= MAX_ITEM_BYTES {
        return Some(item);
    }
    // Preserve typed provenance; oversized tool payloads/images are omitted
    // from extraction, while original history stays intact in session storage.
    let excerpt = item.text_content().chars().take(4000).collect::<String>();
    match &mut item {
        ConversationItem::User(user) => {
            user.content = vec![echo_agent_sampling_types::ContentPart::Text {
                text: excerpt.into(),
            }];
        }
        ConversationItem::Assistant(assistant) => {
            assistant.content = excerpt.into();
        }
        _ => return None,
    }
    (serialized_size(&item) <= MAX_ITEM_BYTES).then_some(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_single_user_turn_has_hard_budgets_and_keeps_latest_outcome() {
        let mut messages = vec![ConversationItem::user("original user request")];
        for _ in 0..190 {
            messages.push(ConversationItem::assistant("x".repeat(100_000)));
        }
        messages.push(ConversationItem::assistant("final outcome"));
        let window = select_flush_window(messages, 80);
        assert!(window.len() <= MAX_FLUSH_MESSAGES);
        assert!(window.iter().map(serialized_size).sum::<usize>() <= MAX_FLUSH_BYTES);
        assert_eq!(window[0].text_content(), "original user request");
        assert_eq!(window.last().unwrap().text_content(), "final outcome");
        assert!(select_flush_window(Vec::new(), 80).is_empty());
        assert!(select_flush_window(vec![ConversationItem::user("test")], 0).is_empty());
    }

    #[test]
    fn test_select_flush_window_expands_to_user_boundary() {
        let mut messages = vec![ConversationItem::user("early question")];
        for i in 0..20 {
            messages.push(ConversationItem::assistant(format!("response {i}")));
        }

        let window = select_flush_window(messages, 20);

        assert_eq!(window.len(), 21);
        assert!(matches!(window[0], ConversationItem::User(_)));
    }

    #[test]
    fn test_select_flush_window_filters_system_messages() {
        let messages = vec![
            ConversationItem::system("you are helpful"),
            ConversationItem::user("hi"),
            ConversationItem::assistant("hello"),
        ];

        let window = select_flush_window(messages, 20);

        assert!(
            window
                .iter()
                .all(|item| !matches!(item, ConversationItem::System(_)))
        );
        assert_eq!(window.len(), 2);
    }

    #[test]
    fn test_select_flush_window_short_conversation() {
        let messages = vec![
            ConversationItem::user("hi"),
            ConversationItem::assistant("hello"),
        ];

        let window = select_flush_window(messages, 20);

        assert_eq!(window.len(), 2);
        assert!(matches!(window[0], ConversationItem::User(_)));
    }

    #[test]
    fn flush_window_preserves_agent_provenance_until_request_projection() {
        let human = ConversationItem::user("human request");
        let agent = ConversationItem::agent_message("agent context");
        let window = select_flush_window(vec![human.clone(), agent], 20);
        let projected =
            echo_agent_chat_state::compaction_utils::ModelRequestHistory::from_raw(window)
                .into_items();

        assert_eq!(projected[0].text_content(), human.text_content());
        assert_eq!(
            projected[1].text_content(),
            format!(
                "{}\nagent context",
                echo_agent_chat_state::compaction_utils::AGENT_MESSAGE_MODEL_LABEL
            )
        );
    }
}
