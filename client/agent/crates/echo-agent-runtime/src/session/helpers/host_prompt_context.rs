//! Host capability instructions travel separately from user command text.
//! They are added only to a real model prompt, after slash resolution and
//! large-input truncation, so they cannot become command arguments or vanish
//! together with the end of a long user message.
use echo_agent_sampling_types::{ContentPart, ConversationItem};
use serde_json::{Map, Value};

pub const AUTOMATION_CONTEXT_KEY: &str = "echoAgentAutomationContext";
pub const PROJECT_CONTEXT_KEY: &str = "echoAgentProjectContext";
pub const MAX_CONTEXT_BYTES: usize = 128 * 1024;

pub fn automation_context(meta: &Map<String, Value>) -> Option<&str> {
    context(meta, AUTOMATION_CONTEXT_KEY)
}

pub fn take_project_context(
    blocks: &mut [agent_client_protocol::ContentBlock],
    allowed: bool,
) -> Option<String> {
    let mut context = None;
    for block in blocks {
        let agent_client_protocol::ContentBlock::Text(text) = block else {
            continue;
        };
        let Some(meta) = text.meta.as_mut() else {
            continue;
        };
        if let Some(Value::String(value)) = meta.remove(PROJECT_CONTEXT_KEY) {
            if allowed
                && context.is_none()
                && !value.trim().is_empty()
                && value.len() <= MAX_CONTEXT_BYTES
            {
                context = Some(value);
            }
        }
    }
    context
}

fn context<'a>(meta: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    meta.get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty() && text.len() <= MAX_CONTEXT_BYTES)
}

pub fn append_model_context(message: &mut String, context: Option<&str>) {
    if let Some(context) = context {
        message.push_str("\n\n");
        message.push_str(context);
    }
}

/// Add current project guidance to the model request without storing it as a
/// historical user message that would follow the session into another project.
pub(crate) fn apply_project_context(items: &mut Vec<ConversationItem>, context: &str) {
    let latest_prompt = items.iter_mut().rev().find_map(|item| match item {
        ConversationItem::User(user)
            if user.prompt_index.is_some() && user.synthetic_reason.is_none() =>
        {
            Some(user)
        }
        _ => None,
    });
    if let Some(user) = latest_prompt {
        user.content.push(ContentPart::Text {
            text: format!("\n\n{context}").into(),
        });
    } else {
        let position = usize::from(matches!(items.first(), Some(ConversationItem::System(_))));
        items.insert(
            position,
            ConversationItem::system_reminder(format!("<system-reminder>\n{context}\n</system-reminder>")),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_project_guidance_is_request_local_across_project_moves() {
        let mut first = ConversationItem::user("first task");
        first.set_prompt_index(1);
        let mut next = ConversationItem::user("follow-up task");
        next.set_prompt_index(2);
        let mut items = vec![first];
        let mut request = items.clone();
        apply_project_context(&mut request, "project A");
        assert!(request[0].text_content().contains("project A"));
        assert_eq!(items[0].text_content(), "first task");

        items.push(ConversationItem::assistant("first result"));
        items.push(next);
        items.push(ConversationItem::system_reminder("recent reminder"));
        let mut moved = items.clone();
        apply_project_context(&mut moved, "project B");
        assert_eq!(moved[0].text_content(), "first task");
        assert!(moved[2].text_content().contains("project B"));
        assert_eq!(items[2].text_content(), "follow-up task");
        assert_eq!(moved[3].text_content(), "recent reminder");
    }

    #[test]
    fn escaped_task_scope_keeps_its_current_turn_even_after_large_input_truncation() {
        let context =
            serde_json::json!({"turnId":"current", "previousTask":"\"\\".repeat(8192)}).to_string();
        let meta = Map::from_iter([(
            AUTOMATION_CONTEXT_KEY.into(),
            Value::String(context.clone()),
        )]);
        // The user-input truncator has already replaced a long source prompt.
        let mut model_message = "Read the full user input from /tmp/input.txt".to_owned();
        append_model_context(&mut model_message, automation_context(&meta));
        assert!(model_message.ends_with(&context));
        assert!(model_message.contains("\"turnId\":\"current\""));
    }

    #[test]
    fn malformed_or_unbounded_metadata_cannot_expand_model_context() {
        for value in [
            Value::Bool(true),
            Value::String(" ".into()),
            Value::String("x".repeat(MAX_CONTEXT_BYTES + 1)),
        ] {
            assert!(
                automation_context(&Map::from_iter([(AUTOMATION_CONTEXT_KEY.into(), value)]))
                    .is_none()
            );
        }
    }
}
