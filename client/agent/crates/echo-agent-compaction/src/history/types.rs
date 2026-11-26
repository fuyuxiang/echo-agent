//! Shared types for conversation history compaction.

use serde::{Deserialize, Serialize};

/// Strategy for how conversation compaction is performed.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    /// Send all turns to the LLM in one shot (original behaviour).
    #[default]
    Basic,
    /// Divide turns into ≤ `dnc_chunk_token_limit` chunks, compact each,
    /// then combine the summaries into a final compaction.
    DivideAndConquer,
    /// echo-agent-build style full-replace summarization: summarize the selected
    /// persisted history range with the code-compaction full-replace prompt and
    /// persist the summary as the durable conversation compaction overlay.
    FullReplace,
}

impl CompactionStrategy {
    /// Stable, low-cardinality metric label for this strategy.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::DivideAndConquer => "divide_and_conquer",
            Self::FullReplace => "full_replace",
        }
    }
}
