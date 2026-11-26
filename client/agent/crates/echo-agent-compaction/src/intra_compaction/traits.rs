//! Trait abstractions for intra-compaction.

use async_trait::async_trait;

use super::trigger::IntraCompactionError;

/// Which segment of the conversation a single intra-compaction pass acts on.
///
/// Determines the prompt template the orchestrator uses, which read-view
/// it pulls items from on the stream processor (`get_accumulated_turns_for_compaction`
/// vs `get_history_turns_for_compaction`), and which branch the stream processor's
/// [`CompactionStreamProc::replace_with_compaction`] dispatches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTarget {
    /// Compact the agent loop's accumulated step turns (assistant outputs,
    /// tool calls, tool results). Fine-grained prompt.
    Steps,
    /// Compact prior conversation-history turns (user/assistant exchanges
    /// from before the current agent loop). Coarser prompt, shared with
    /// inter-compaction.
    History,
    /// Replace the *whole* conversation (prior history + accumulated steps)
    /// with a single summary — echo-agent-build's full-replace strategy. No tail is
    /// kept; the read-view is [`CompactionStreamProc::get_all_turns_for_compaction`].
    FullReplace,
}

impl CompactionTarget {
    /// Stable metric label for this target.
    pub fn label(self) -> &'static str {
        match self {
            Self::Steps => "steps",
            Self::History => "history",
            Self::FullReplace => "full_replace",
        }
    }
}

/// Minimal interface the compaction orchestrator needs from the agent's
/// stream processor. Implemented by EchoAgent chat's
/// `StreamProcessor` (`Item = Arc<EchoAgentTurn>`).
///
/// Two read-views are exposed:
///
/// - **Accumulated step turns**: items added since the agent loop started
///   — assistant outputs, tool calls, tool results, recovery turns. The
///   original conversation (system prompt, user messages, prior history)
///   is excluded. Used by step (fine-grained) compaction.
/// - **History turns**: items from prior user-query/assistant-response
///   exchanges, before the current agent loop began. Used by history
///   (coarse) compaction.
///
/// The single mutator [`Self::replace_with_compaction`] takes a
/// [`CompactionTarget`] and dispatches internally to the steps- or
/// history-specific path. It is the final step of a compaction cycle:
/// the LLM-produced summary is committed into parser state. The
/// orchestrator [`super::apply_intra_compaction`] and its peers
/// [`super::apply_steps_compaction`] / [`super::apply_history_compaction`]
/// are the layers above that produce the summary and call this method.
///
/// Implementations that don't support a particular target return
/// [`IntraCompactionError::Unsupported`] from the matching match arm.
#[async_trait]
pub trait CompactionStreamProc: Send + Sync {
    /// The harness's conversation item type.
    type Item;

    /// Get the items accumulated across all completed steps, oldest first.
    /// Candidates for **steps** compaction.
    async fn get_accumulated_turns_for_compaction(&self) -> Vec<Self::Item>;

    /// Get the conversation-history items (prior user/assistant exchanges
    /// from before the current agent loop), oldest first. Candidates for
    /// **history** compaction.
    ///
    /// Default impl returns empty — implementations that do not support
    /// history compaction will have nothing to compact.
    async fn get_history_turns_for_compaction(&self) -> Vec<Self::Item> {
        Vec::new()
    }

    /// Get the **whole** conversation — prior history followed by the
    /// accumulated step turns, oldest first. Candidates for **full-replace**
    /// (`CompactionTarget::FullReplace`) compaction.
    ///
    /// The default composes the two read-views above (`history ++ steps`),
    /// which is correct for any implementation; override only if a harness can
    /// produce the combined view more cheaply.
    ///
    /// The `Self::Item: Send` bound lets the default hold the history vec across
    /// the second `await` while keeping the boxed future `Send`; every concrete
    /// item type (`Arc<EchoAgentTurn>`) already satisfies it.
    async fn get_all_turns_for_compaction(&self) -> Vec<Self::Item>
    where
        Self::Item: Send,
    {
        let mut all = self.get_history_turns_for_compaction().await;
        all.extend(self.get_accumulated_turns_for_compaction().await);
        all
    }

    /// Top-level intra-compaction mutator. Replaces the first
    /// `n_turns_to_remove` items in the read-view selected by `target` with
    /// the single given `compaction_turn`.
    ///
    /// Implementations dispatch internally on `target` to the steps or
    /// history specific path. On invalid input
    /// (`n_turns_to_remove > view.len()`), returns
    /// [`IntraCompactionError::InvalidSplit`] and leaves state untouched.
    async fn replace_with_compaction(
        &self,
        target: CompactionTarget,
        n_turns_to_remove: usize,
        compaction_turn: Self::Item,
    ) -> Result<(), IntraCompactionError>;
}
