//! Token-count seam.
//!
//! Budgeting math in the shared engine needs a *trusted* token count, but the
//! two harnesses disagree on how to produce one:
//!
//! - EchoAgent chat has a real tokenizer (`TextTokenizer` / `ImageTokenizer`) and
//!   counts whole turns via `EchoAgentTurn::get_num_tokens`.
//! - echo-agent-build estimates with `bytes / 4`.
//!
//! Rather than bake either policy into the shared crate, callers supply an
//! [`ItemTokenCounter`]. This keeps the engine deterministic and testable
//! while letting each harness plug in its own counting strategy.
//!
//! There is intentionally **no** blanket `Arc` forwarding here: each harness
//! implements the counter directly for the item type its algorithms run on
//! (EchoAgent chat: `ItemTokenCounter<Arc<EchoAgentTurn>>`), so exactly one mechanism
//! is in play.

/// Counts tokens for a single conversation item on behalf of the shared
/// budgeting logic.
pub trait ItemTokenCounter<T: ?Sized>: Send + Sync {
    /// Trusted token count of `item`.
    fn count_item_tokens(&self, item: &T) -> u32;
}
