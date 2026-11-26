//! @-provider: fuzzy file completion for `@foo/bar` references.
//!
//! # Architecture
//!
//! - [`context`] — parses `@query` tokens from text + cursor position
//! - [`state`] — owns the fuzzy matcher daemon, results, and dropdown state
//! - [`dropdown`] — dropdown list rendering (ListPane wrapper, Phase 1)
//! - [`line_viewer`] — centered popup file viewer (Phase 3, not yet implemented)
//! - [`preview`] — file preview alongside dropdown (Phase 4, not yet implemented)

pub mod context;
pub mod dropdown;
pub mod line_viewer;
mod state;

pub use context::AtContext;
pub use state::{FileSearchReplacement, FileSearchState};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Build a styled `@path` or `@path:N-M` display line.
///
/// Style: `@` and `:` in `theme.gray`, path in `theme.path`, numbers in `theme.gray_bright`.
/// Set `at_prefix` to include the leading `@` (prompt chip) or omit it (viewer title).
/// Used by both the prompt element chip and the line viewer title bar.
pub fn styled_file_ref<'a>(
    path: &str,
    line_range: Option<&str>,
    theme: &Theme,
    at_prefix: bool,
) -> Line<'a> {
    let dim = Style::default().fg(theme.gray);
    let path_style = Style::default().fg(theme.path);
    let num_style = Style::default().fg(theme.gray_bright);

    let mut spans = Vec::new();
    if at_prefix {
        spans.push(Span::styled("@", dim));
    }
    spans.push(Span::styled(path.to_owned(), path_style));
    if let Some(range) = line_range {
        spans.push(Span::styled(":", dim));
        spans.push(Span::styled(range.to_owned(), num_style));
    }
    Line::from(spans)
}
