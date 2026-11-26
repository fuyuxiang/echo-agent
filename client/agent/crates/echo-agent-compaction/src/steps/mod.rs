//! Steps compaction — prompt content for compacting accumulated step
//! turns (tool calls + assistant responses) within a single agent turn.
//!
//! Parallel to [`crate::history`] (the history-compaction content): this is the
//! *steps* side. The orchestration that uses it lives in
//! [`crate::intra_compaction`] (the `Steps` target / `StepsOnly` mode), and the
//! turn selection it shares with the History target is the crate-root
//! [`select`](crate::select) primitive (not steps-specific).

pub mod prompt;

pub use prompt::format_compaction_prompt;
