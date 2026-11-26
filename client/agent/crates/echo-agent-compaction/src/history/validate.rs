//! Compaction result validation (text-level, harness-agnostic).
//!
//! The EchoAgent chat's `validate_compaction_result(EchoAgentMessage, …)` wrapper in
//! the harness crate extracts the message text and delegates here.

use super::types::CompactionStrategy;

/// Errors from validating a compaction result before persisting.
#[derive(Debug)]
pub enum CompactionValidationError {
    /// The compaction output has no text content. Persisting an empty
    /// summary would be silently skipped on hydration while blocking
    /// future compaction triggers.
    EmptyContent,
    /// DivideAndConquer `<chunk_summary>` XML tags are not balanced, indicating
    /// the LLM output was truncated or malformed. The content may be partially
    /// usable but signals an incomplete compaction.
    UnbalancedChunkTags { open: usize, close: usize },
}

impl std::fmt::Display for CompactionValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyContent => write!(f, "compaction message has empty text content"),
            Self::UnbalancedChunkTags { open, close } => {
                write!(
                    f,
                    "unbalanced chunk_summary tags: {} open, {} close",
                    open, close
                )
            }
        }
    }
}

/// Validate compaction output text before persisting.
///
/// Checks:
/// 1. Non-empty text content — an empty compaction would be silently skipped
///    on hydration while blocking future compaction triggers.
/// 2. DivideAndConquer: balanced `<chunk_summary>` tags — unbalanced tags
///    indicate truncated LLM output.
pub fn validate_compaction_text(
    text_content: &str,
    strategy: &CompactionStrategy,
) -> Result<(), CompactionValidationError> {
    // 1. Non-empty text content
    if text_content.trim().is_empty() {
        return Err(CompactionValidationError::EmptyContent);
    }

    // 2. DnC: validate chunk_summary tags are balanced
    if matches!(strategy, CompactionStrategy::DivideAndConquer) {
        let open_count = text_content.matches("<chunk_summary").count();
        let close_count = text_content.matches("</chunk_summary>").count();
        if open_count != close_count {
            return Err(CompactionValidationError::UnbalancedChunkTags {
                open: open_count,
                close: close_count,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_content_rejected() {
        assert!(matches!(
            validate_compaction_text("", &CompactionStrategy::Basic),
            Err(CompactionValidationError::EmptyContent)
        ));
        assert!(matches!(
            validate_compaction_text("  \n ", &CompactionStrategy::DivideAndConquer),
            Err(CompactionValidationError::EmptyContent)
        ));
    }

    #[test]
    fn valid_basic_accepted() {
        assert!(validate_compaction_text("A valid summary", &CompactionStrategy::Basic).is_ok());
    }

    #[test]
    fn unbalanced_dnc_tags_rejected() {
        let text = "<chunk_summary index=\"0\">\nsummary\n</chunk_summary>\n<chunk_summary index=\"1\">\nmissing close";
        assert!(matches!(
            validate_compaction_text(text, &CompactionStrategy::DivideAndConquer),
            Err(CompactionValidationError::UnbalancedChunkTags { open: 2, close: 1 })
        ));
    }

    #[test]
    fn balanced_dnc_tags_accepted() {
        let text = "<chunk_summary index=\"0\">\nsummary 0\n</chunk_summary>\n<chunk_summary index=\"1\">\nsummary 1\n</chunk_summary>";
        assert!(validate_compaction_text(text, &CompactionStrategy::DivideAndConquer).is_ok());
    }

    #[test]
    fn basic_ignores_unbalanced_tags() {
        let text = "<chunk_summary index=\"0\">no close tag";
        assert!(validate_compaction_text(text, &CompactionStrategy::Basic).is_ok());
    }
}
