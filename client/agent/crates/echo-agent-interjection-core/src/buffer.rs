use serde::{Deserialize, Serialize};

use crate::events::EventQueue;
use crate::format::format_interjection;

/// A scoped entry belongs exclusively to this prompt; if it misses that
/// prompt's drain point, its host retains responsibility for continuation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptScope {
    pub prompt_id: String,
    pub interjection_id: String,
}

/// A buffered mid-turn interjection awaiting the next safe drain point.
/// `Attachment` is host-defined (inline images, asset IDs); core never reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingInterjection<Attachment> {
    pub text: String,
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub scope: Option<PromptScope>,
}

/// A drained entry, wrapped and ready to emit as a synthetic user message.
#[derive(Debug, Clone, PartialEq)]
pub struct FormattedInterjection<Attachment> {
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub scope: Option<PromptScope>,
}

/// A queue of pending interjections — just an [`EventQueue`] of
/// [`PendingInterjection`]. Use [`drain_formatted`] to drain + frame them as
/// synthetic user messages.
pub type InterjectionBuffer<Attachment> = EventQueue<PendingInterjection<Attachment>>;

/// Drain `buffer`, framing each entry as a synthetic user message (FIFO, one
/// message per entry, never merged). `sanitize_text` runs on the raw text first
/// (hosts strip artifacts like image placeholder paths; pass
/// `std::convert::identity` if none).
pub fn drain_formatted<Attachment>(
    buffer: &InterjectionBuffer<Attachment>,
    sanitize_text: impl Fn(String) -> String,
) -> Vec<FormattedInterjection<Attachment>> {
    buffer
        .drain_all()
        .into_iter()
        .map(|entry| FormattedInterjection {
            text: format_interjection(sanitize_text(entry.text)),
            attachments: entry.attachments,
            scope: entry.scope,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_formatted_sanitizes_wraps_and_preserves_order() {
        let buf: InterjectionBuffer<()> = InterjectionBuffer::new();
        buf.push(PendingInterjection {
            scope: None,
            text: "look at [SECRET] one".into(),
            attachments: vec![],
        });
        buf.push(PendingInterjection {
            scope: None,
            text: "two".into(),
            attachments: vec![],
        });

        let out = drain_formatted(&buf, |t| t.replace("[SECRET] ", ""));
        assert!(buf.is_empty());
        assert_eq!(out.len(), 2, "one message per entry, never merged");
        assert!(out[0]
            .text
            .contains("<user_query>\nlook at one\n</user_query>"));
        assert!(out[1].text.contains("<user_query>\ntwo\n</user_query>"));
        assert!(out[0]
            .text
            .starts_with("The user sent a message while you were working:"));
    }
}

#[cfg(test)]
mod scoped_tests {
    use super::*;
    #[test]
    fn formatting_preserves_prompt_scope_without_changing_ordinary_entries() {
        let scope = PromptScope {
            prompt_id: "prompt".into(),
            interjection_id: "correction".into(),
        };
        let buffer: InterjectionBuffer<()> = InterjectionBuffer::new();
        buffer.push(PendingInterjection {
            text: "ordinary".into(),
            attachments: vec![],
            scope: None,
        });
        buffer.push(PendingInterjection {
            text: "correction".into(),
            attachments: vec![],
            scope: Some(scope.clone()),
        });
        let formatted = drain_formatted(&buffer, std::convert::identity);
        assert_eq!(formatted[0].scope, None);
        assert_eq!(formatted[1].scope, Some(scope));
        assert!(formatted[0].text.contains("ordinary"));
        assert!(formatted[1].text.contains("correction"));
    }
}
