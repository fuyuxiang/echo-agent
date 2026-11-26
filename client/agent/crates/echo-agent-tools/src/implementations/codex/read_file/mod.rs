//! Codex `read_file` — text file reader in codex `L{n}: {content}` format.
//!
//! This module ports the codex read_file tool as a separate tool under
//! `ToolNamespace::Codex`. It supports two modes:
//!
//! - **Slice mode** — reads a contiguous range of lines (default).
//! - **Indentation mode** — reads a block based on indentation structure.
//!
//! # Submodules
//!
//! - [`text_utils`] — shared text helpers (char-boundary truncation).
//! - [`slice`] — slice-mode reader (exact port of codex `slice::read()`).
//! - [`indentation`] — indentation-mode reader (exact port of codex `indentation::*`).
//! - [`tool`] — `CodexReadFileTool` implementation, input types, description.

pub mod indentation;
pub mod slice;
pub(crate) mod text_utils;
pub mod tool;

// Re-exports for convenience.
pub use tool::{CodexReadFileInput, CodexReadFileTool};
