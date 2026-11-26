//! Checkpoint types for incremental markdown rendering.
//!
//! This module defines types for identifying stable boundaries in markdown text
//! where rendered output can be "frozen" and cached. Content before a checkpoint
//! will not change regardless of what text is appended after it.
//!
//! # Design
//!
//! Checkpoints are only created at **top-level** (depth=0) block boundaries. Blocks
//! nested inside lists, blockquotes, or tables cannot be checkpoints because the
//! outer container might continue.
//!
//! # Example
//!
//! ```text
//! # Heading          <- Checkpoint after this (heading at depth=0)
//!
//! Paragraph text.    <- Checkpoint after blank line (paragraph at depth=0)
//!
//! - List item        <- NO checkpoint (inside list)
//!   ```code```       <- NO checkpoint (code block inside list)
//! - Another item
//!                    <- Checkpoint here (list closed at depth=0)
//! ```

/// A position in the source text where rendered content can be frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    /// Byte offset in source text (exclusive end of frozen region).
    /// Content in `text[..source_bytes]` can be cached.
    pub source_bytes: usize,
    /// Number of output lines that correspond to this checkpoint.
    /// Lines `0..output_lines` can be frozen.
    pub output_lines: usize,
    /// What kind of block ended at this checkpoint.
    pub kind: CheckpointKind,
}

/// The type of markdown block that created a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointKind {
    /// A heading (any level: h1-h6)
    Heading,
    /// A paragraph followed by a blank line
    Paragraph,
    /// A fenced or indented code block
    CodeBlock,
    /// A blockquote that closed at top level
    BlockQuote,
    /// A list (ordered or unordered) that closed at top level
    List,
    /// A thematic break (horizontal rule: ---, ***, ___)
    ThematicBreak,
    /// A table that closed at top level
    Table,
    /// A raw HTML block
    HtmlBlock,
}
