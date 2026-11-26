//! Prompt construction for **steps** compaction.
//!
//! The step-level intra-compaction prompt: short and focused on summarising
//! tool-call history mid-task. Parallel to [`crate::history::prompt`] (the
//! history-compaction prompts); templates live in the crate-root `templates/`.

use crate::prompt::CompactionPrompt;

/// Build the standard prompt for step-level intra-compaction.
///
/// The prompts are short and focused on summarising tool-call history
/// mid-task — the assistant has already done several steps of work and
/// we need to free up context so it can continue.
pub fn format_compaction_prompt() -> CompactionPrompt {
    CompactionPrompt {
        system: include_str!("../templates/intra_compaction_system.txt").to_string(),
        user: include_str!("../templates/intra_compaction_user.txt").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_non_empty() {
        let p = format_compaction_prompt();
        assert!(!p.system.trim().is_empty(), "system prompt empty");
        assert!(!p.user.trim().is_empty(), "user prompt empty");
    }
}
