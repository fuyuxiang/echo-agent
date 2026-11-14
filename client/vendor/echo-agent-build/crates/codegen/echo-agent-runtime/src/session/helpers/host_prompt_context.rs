//! Host capability instructions travel separately from user command text.
//! They are added only to a real model prompt, after slash resolution and
//! large-input truncation, so they cannot become command arguments or vanish
//! together with the end of a long user message.
use serde_json::{Map, Value};

pub const AUTOMATION_CONTEXT_KEY: &str = "echoAgentAutomationContext";
const MAX_CONTEXT_BYTES: usize = 128 * 1024;

pub fn automation_context(meta: &Map<String, Value>) -> Option<&str> {
    meta.get(AUTOMATION_CONTEXT_KEY)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty() && text.len() <= MAX_CONTEXT_BYTES)
}

pub fn append_model_context(message: &mut String, context: Option<&str>) {
    if let Some(context) = context {
        message.push_str("\n\n");
        message.push_str(context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
