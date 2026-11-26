//! Conservative input estimate for request admission. Images use a vision
//! allowance; base64 transport bytes are never treated as prompt text.
use super::{ContentPart, ConversationItem, ConversationRequest};

/// More conservative than bytes/4 for Chinese text, punctuation and code.
fn text_tokens(text: &str) -> u64 {
    let mut ascii = 0;
    let mut tokens = 0;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c.is_ascii_whitespace() {
            ascii += 1;
        } else if c.is_ascii() {
            tokens += 1;
        } else {
            tokens += 2;
        }
    }
    tokens + (ascii + 2) / 3
}

pub fn estimate_request_tokens(request: &ConversationRequest) -> u64 {
    let mut count = text_tokens(&serde_json::to_string(&request.tools).unwrap_or_default())
        + text_tokens(&format!("{:?}", request.hosted_tools))
        + request
            .json_schema
            .as_ref()
            .map(|schema| text_tokens(&schema.to_string()))
            .unwrap_or(0);
    for item in &request.items {
        count += 12;
        match item {
            ConversationItem::ToolResult(t) => {
                count += text_tokens(&t.content) + t.images.len() as u64 * 1600;
            }
            ConversationItem::System(s) => count += text_tokens(&s.content),
            ConversationItem::User(u) => {
                for part in &u.content {
                    count += match part {
                        ContentPart::Text { text } => text_tokens(text),
                        ContentPart::Image { .. } => 1600,
                    };
                }
            }
            ConversationItem::Assistant(a) => {
                count += text_tokens(&a.content)
                    + text_tokens(&serde_json::to_string(&a.tool_calls).unwrap_or_default())
            }
            // Serialize the other variants so tool-call arguments, reasoning
            // and system instructions participate in the budget as well.
            _ => count += text_tokens(&serde_json::to_string(item).unwrap_or_default()),
        }
    }
    count
}
