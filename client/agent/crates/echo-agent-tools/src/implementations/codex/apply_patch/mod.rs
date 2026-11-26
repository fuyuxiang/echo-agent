//! Codex `apply_patch` — core patch engine (pure library, no I/O).
//!
//! This module ports the codex patch parser, fuzzy matcher, and replacement
//! logic as pure functions with zero filesystem dependencies. All I/O
//! (reading/writing files) is handled by the tool layer in a later milestone.
//!
//! # Submodules
//!
//! - [`apply`] — `derive_new_contents()`, `compute_replacements()`,
//!   `apply_replacements()` — all accept `&str` input.
//! - [`errors`] — `ApplyPatchError`, `ParseError`.
//! - [`parser`] — `parse_patch()`, `Hunk`, `UpdateFileChunk`.
//! - [`seek_sequence`] — 4-tier fuzzy line matcher.

pub mod apply;
pub mod errors;
pub mod parser;
pub mod seek_sequence;
pub mod tool;

// Re-exports for convenience.
pub use apply::derive_new_contents;
pub use errors::{ApplyPatchError, ParseError};
pub use parser::{Hunk, ParsedPatch, UpdateFileChunk, parse_patch};
pub use tool::{ApplyPatchInput, ApplyPatchTool};
