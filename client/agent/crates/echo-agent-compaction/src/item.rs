//! Data abstraction — the `CompactionItem` seam.
//!
//! The shared compaction algorithms operate over a sequence of *items*
//! (turns/messages) without knowing the concrete harness type. The chat
//! harness implements [`CompactionItem`] for its `EchoAgentTurn`;
//! echo-agent-build implements it for `echo_agent_sampling_types::ConversationItem`.
//!
//! Keeping the contract minimal is deliberate: the algorithms only need
//! enough structure to (a) classify roles, (b) read text, and (c) preserve
//! the tool-request/tool-result pairing invariant when selecting a split
//! point (an `Assistant(tool_request)` and the `Tool` results that satisfy
//! it must never be separated, or the model API rejects the orphaned tool
//! results with a 400).
//!
//! [`CompactionItemBuilder`] is the *constructive* extension used by the
//! history-compaction algorithms that need to rebuild items (strip prior
//! `<echo_agent_user_queries>` blocks, drop tool content from assistant turns,
//! wrap an LLM summary into a carrier item).

/// Harness-agnostic role of a single conversation item.
///
/// This is the common denominator of `EchoAgentRole` (EchoAgent chat) and the
/// `ConversationItem` variants (echo-agent-build).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionRole {
    /// System prompt.
    System,
    /// Developer prompt (EchoAgent chat) — maps to System on harnesses without a
    /// distinct developer role.
    Developer,
    /// A user message.
    User,
    /// An assistant output (may carry tool requests).
    Assistant,
    /// A tool result.
    Tool,
}

/// A file attached to a user item, as seen by the shared user-query
/// extraction (`<echo_agent_file id=".." name=".." />` lines in the
/// `<echo_agent_user_queries>` preamble).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionFileRef {
    /// Stable unique id of the attachment source.
    pub id: String,
    /// Human-readable file name.
    pub name: String,
}

/// Contract: one turn/item in a conversation, as seen by the shared
/// compaction algorithms.
///
/// Implementors:
/// - EchoAgent chat: `EchoAgentTurn`
/// - echo-agent-build: `ConversationItem`
pub trait CompactionItem {
    /// The harness-agnostic role of this item.
    fn role(&self) -> CompactionRole;

    /// The item's text content, if any. Tool results and assistant tool-only
    /// turns may have no text.
    ///
    /// Returns an owned `String` because some harnesses (EchoAgent chat's
    /// `EchoAgentTurn`) compute the flattened text on demand rather than storing a
    /// borrowable slice.
    fn text(&self) -> Option<String>;

    /// Whether this item is a tool result. Used by the split-point selector to
    /// avoid orphaning tool results from their originating assistant turn.
    fn is_tool_result(&self) -> bool {
        matches!(self.role(), CompactionRole::Tool)
    }

    /// Whether this (assistant) item carries at least one tool request.
    /// `false` for all non-assistant items.
    fn has_tool_requests(&self) -> bool;

    /// Whether this item carries a *prior compaction summary* (EchoAgent chat: a
    /// `Developer` turn with `DeveloperPromptCategory::ConversationCompaction`).
    ///
    /// The basic history filter keeps such items so earlier summaries get
    /// re-summarised instead of dropped, and `separate_prior_user_queries`
    /// strips their `<echo_agent_user_queries>` blocks before sampling.
    ///
    /// Required (no default) on purpose: a forgotten implementation or a
    /// missed `Arc` forwarding would silently drop prior summaries on
    /// re-compaction.
    fn is_compaction_summary(&self) -> bool;

    /// File attachments on a (user) item, for the `<echo_agent_file>` lines in the
    /// `<echo_agent_user_queries>` preamble. Empty for items without attachments.
    ///
    /// Required (no default) for the same reason as
    /// [`Self::is_compaction_summary`]: silent attachment loss on compaction
    /// must be a compile error, not a runtime surprise.
    fn attachment_refs(&self) -> Vec<CompactionFileRef>;
}

/// Constructive extension of [`CompactionItem`] for algorithms that rebuild
/// items (history filtering and summary-carrier construction).
///
/// Not object-safe (`compaction_summary_item` has no receiver) — always used
/// through generics, never as `dyn`.
pub trait CompactionItemBuilder: CompactionItem + Clone {
    /// Construct the item that carries a compaction summary back into the
    /// conversation (EchoAgent chat: a `Developer` turn with category
    /// `ConversationCompaction`). The result must satisfy
    /// `is_compaction_summary() == true`.
    fn compaction_summary_item(text: String) -> Self;

    /// Rebuild this item keeping only user-visible content, dropping tool
    /// requests/results (EchoAgent chat: keep only `Channel` contents of an
    /// assistant turn). Returns `None` when nothing visible remains.
    ///
    /// Only meaningful for `Assistant` items; the shared filters never call
    /// it for other roles, but implementations should return
    /// `Some(self.clone())` for them to keep the contract total.
    fn strip_tool_content(&self) -> Option<Self>;

    /// Truncate this item's payload for **summarizer input** to roughly
    /// `max_tokens` (echo-agent-build style: `max_bytes = max_tokens * 4`, prefix
    /// clip). Used by FullReplace fit for oversized tool results and
    /// emergency tail shrink. Default: clone unchanged.
    ///
    /// Prefer tool-result text; keep structural fields (ids, names). Drop
    /// large sidecars when present.
    fn truncate_payload_for_compaction(&self, max_tokens: u32) -> Self {
        let _ = max_tokens;
        self.clone()
    }
}

/// Write seam for the full-replace **assembler**
/// ([`crate::code_compaction::assemble::assemble_compacted_history`]):
/// constructs the typed harness items that make up echo-agent-build's rebuilt
/// history.
///
/// This is a sibling of [`CompactionItemBuilder`], not a part of it, on
/// purpose. `CompactionItemBuilder` is already implemented by EchoAgent chat's
/// `EchoAgentTurn`; adding these constructors to it as required methods would break
/// that impl. They are also echo-agent-build-specific (EchoAgent chat's tail-keep path
/// has no `user_meta` / `project_instructions` / `system_reminder` carrier
/// concept), so they live in their own seam that only the full-replace
/// assembler depends on.
///
/// The echo-agent-build implementor (`ConversationItem`) maps each constructor to the
/// matching factory so the `SyntheticReason` tags the replay / spawn-time
/// idempotence guards rely on are preserved.
pub trait CompactionItemFactory: Sized {
    /// A real user message (used for the last user query).
    fn new_user(text: String) -> Self;
    /// A synthetic user message carrying compaction metadata (user-info
    /// prefix, summary carrier).
    fn new_user_meta(text: String) -> Self;
    /// A user message carrying project instructions (AGENTS.md), tagged so
    /// spawn-time idempotence guards recognize it on resume.
    fn new_project_instructions(text: String) -> Self;
    /// A synthetic user message carrying a `<system-reminder>` block.
    fn new_system_reminder(text: String) -> Self;
}

/// Forward [`CompactionItem`] through shared references so the algorithms can
/// operate over `&[Arc<T>]` (EchoAgent chat stores turns as `Arc<EchoAgentTurn>`).
impl<T: CompactionItem + ?Sized> CompactionItem for std::sync::Arc<T> {
    fn role(&self) -> CompactionRole {
        (**self).role()
    }
    fn text(&self) -> Option<String> {
        (**self).text()
    }
    fn is_tool_result(&self) -> bool {
        (**self).is_tool_result()
    }
    fn has_tool_requests(&self) -> bool {
        (**self).has_tool_requests()
    }
    fn is_compaction_summary(&self) -> bool {
        (**self).is_compaction_summary()
    }
    fn attachment_refs(&self) -> Vec<CompactionFileRef> {
        (**self).attachment_refs()
    }
}

/// Forward [`CompactionItemBuilder`] through `Arc` — rebuilt items are
/// wrapped in a fresh `Arc`, untouched items are *not* deep-cloned (the
/// shared filters clone the `Arc` pointer directly).
impl<T: CompactionItemBuilder> CompactionItemBuilder for std::sync::Arc<T> {
    fn compaction_summary_item(text: String) -> Self {
        std::sync::Arc::new(T::compaction_summary_item(text))
    }
    fn strip_tool_content(&self) -> Option<Self> {
        (**self).strip_tool_content().map(std::sync::Arc::new)
    }
    fn truncate_payload_for_compaction(&self, max_tokens: u32) -> Self {
        std::sync::Arc::new((**self).truncate_payload_for_compaction(max_tokens))
    }
}
