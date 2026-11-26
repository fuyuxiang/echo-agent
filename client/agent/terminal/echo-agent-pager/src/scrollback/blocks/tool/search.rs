//! SearchToolCallBlock - search/grep for pattern.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{
    AccentStyle, BlockBackground, BlockContext, BlockLine, BlockOutput, DisplayMode, Selectable,
};
use crate::theme::Theme;

use super::TOOL_HEADER_RANGE;
use crate::appearance::AppearanceConfig;

/// A single line match from search results.
#[derive(Debug, Clone)]
pub struct SearchLineMatch {
    /// Line number in the file.
    pub line_number: usize,
    /// Content of the matching line.
    pub content: String,
}

/// A file with its line matches from search results.
#[derive(Debug, Clone)]
pub struct SearchFileMatch {
    /// Path to the file.
    pub path: String,
    /// Line matches within this file.
    pub matches: Vec<SearchLineMatch>,
}

/// Output mode mirroring `OutputMode` from echo-agent-tools.
/// We keep our own copy to avoid pulling in that dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchOutputMode {
    /// Matching lines with context (default).
    #[default]
    Content,
    /// File paths only.
    FilesWithMatches,
    /// Match counts per file.
    Count,
}

impl SearchOutputMode {
    /// Parse from the rawInput `output_mode` string.
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s {
            Some("files_with_matches") => Self::FilesWithMatches,
            Some("count") => Self::Count,
            _ => Self::Content,
        }
    }
}

/// Extra metadata from `GrepSearchInput` — carried for display purposes.
#[derive(Debug, Clone, Default)]
pub struct SearchInputMeta {
    /// Search path (subdirectory), if not workspace root.
    pub path: Option<String>,
    /// Glob filter (e.g. `"*.rs"`).
    pub glob: Option<String>,
    /// Output mode.
    pub output_mode: SearchOutputMode,
    /// Case-insensitive search.
    pub case_insensitive: bool,
    /// File type filter (rg `--type`), e.g. `"rust"`.
    pub file_type: Option<String>,
    /// Multiline regex mode.
    pub multiline: bool,
}

/// Search/grep tool call.
#[derive(Debug, Clone)]
pub struct SearchToolCallBlock {
    /// The search pattern.
    pub pattern: String,
    /// Total number of matches found.
    pub match_count: usize,
    /// Matches grouped by file (line-level matches).
    pub file_matches: Vec<SearchFileMatch>,
    /// File paths only (for `files_with_matches` output mode).
    /// Used when `file_matches` is empty but results exist.
    pub file_paths: Vec<String>,
    /// Error message if the tool call failed (None = success).
    pub error: Option<String>,
    /// Extra metadata from the search input (path, glob, mode, etc.).
    pub meta: SearchInputMeta,
    /// When the tool started running (Phase 2: time tracking).
    pub started_at: Option<std::time::Instant>,
    /// Elapsed time in ms after completion (Phase 2: time tracking).
    pub elapsed_ms: Option<i64>,
}

impl SearchToolCallBlock {
    /// Create a new search block.
    ///
    /// Pre-completed blocks have no meaningful local timing — `started_at`
    /// is `None`. Timing is only set for blocks that enter a running UI
    /// state (via `set_last_running(true)` in `ScrollbackState`).
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            match_count: 0,
            file_matches: Vec::new(),
            file_paths: Vec::new(),
            error: None,
            meta: SearchInputMeta::default(),
            started_at: None,
            elapsed_ms: None,
        }
    }

    /// Set match count and file matches.
    pub fn with_matches(mut self, match_count: usize, file_matches: Vec<SearchFileMatch>) -> Self {
        self.match_count = match_count;
        self.file_matches = file_matches;
        self
    }

    /// Set error (marks as failed).
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Check if successful (no error).
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Set error (mutable) — compute elapsed time if not already set (Phase 2).
    pub fn set_error(&mut self, error: Option<String>) {
        if self.elapsed_ms.is_none()
            && let Some(start) = self.started_at
        {
            self.elapsed_ms = Some(start.elapsed().as_millis() as i64);
        }
        self.error = error;
    }

    /// Finalize elapsed time from `started_at`.
    ///
    /// Idempotent: no-op if `started_at` is `None` (pre-completed block)
    /// or if `elapsed_ms` is already set (already finalized).
    pub fn finish(&mut self) {
        if self.elapsed_ms.is_some() {
            return;
        }
        if let Some(start) = self.started_at {
            self.elapsed_ms = Some(start.elapsed().as_millis() as i64);
        }
    }

    /// Get elapsed time in ms (Phase 2).
    pub fn elapsed_ms(&self) -> Option<i64> {
        match self.elapsed_ms {
            Some(ms) => Some(ms),
            None => self
                .started_at
                .map(|start| start.elapsed().as_millis() as i64),
        }
    }

    /// Set file matches (mutable).
    pub fn set_file_matches(&mut self, match_count: usize, file_matches: Vec<SearchFileMatch>) {
        self.match_count = match_count;
        self.file_matches = file_matches;
    }

    /// Build the match summary string, adapted by output mode.
    ///
    /// - `Content`:            `(3 matches in 2 files)` / `(1 match)` / `(no matches)`
    /// - `FilesWithMatches`:   `(3 files)` / `(1 file)` / `(no matches)`
    /// - `Count`:              `(42 matches across 5 files)` / `(no matches)`
    fn match_summary(&self) -> String {
        if self.match_count == 0 {
            return match self.meta.output_mode {
                SearchOutputMode::FilesWithMatches => "(no files)".to_string(),
                _ => "(no matches)".to_string(),
            };
        }
        match self.meta.output_mode {
            SearchOutputMode::Content => {
                let file_count = self.file_matches.len();
                if file_count > 1 {
                    format!("({} matches in {} files)", self.match_count, file_count)
                } else if self.match_count == 1 {
                    "(1 match)".to_string()
                } else {
                    format!("({} matches)", self.match_count)
                }
            }
            SearchOutputMode::FilesWithMatches => {
                let n = self.match_count; // match_count = # of files in this mode
                if n == 1 {
                    "(1 file)".to_string()
                } else {
                    format!("({n} files)")
                }
            }
            SearchOutputMode::Count => {
                let file_count = self.file_paths.len().max(self.file_matches.len());
                if file_count > 1 {
                    format!("({} matches across {} files)", self.match_count, file_count)
                } else if self.match_count == 1 {
                    "(1 match)".to_string()
                } else {
                    format!("({} matches)", self.match_count)
                }
            }
        }
    }

    /// Whether the pattern is trivial (`"."` or empty) — meaning the glob
    /// is the real search term when present.
    fn is_trivial_pattern(&self) -> bool {
        self.pattern.is_empty() || self.pattern == "."
    }

    /// Render the header line.
    ///
    /// Three cases:
    /// 1. Trivial pattern + glob → `Search glob in path (summary)`
    ///    glob is string-styled without quotes (it IS the search term).
    /// 2. Real pattern + glob → `Search "pattern" in glob in path (summary)`
    ///    glob shown as path scope after first "in".
    /// 3. No glob → `Search "pattern" in path (summary)`
    fn header_line(
        &self,
        theme: &Theme,
        muted: bool,
        dim_details: bool,
        width: Option<usize>,
    ) -> Line<'static> {
        let text_style = if muted {
            theme.muted()
        } else {
            theme.primary()
        };
        let bold_style = text_style.add_modifier(Modifier::BOLD);
        let pattern_style = if muted {
            theme.muted()
        } else {
            theme.fg(theme.accent_success)
        };
        let detail_style = if dim_details {
            theme.dim()
        } else {
            theme.muted()
        };
        let path_style = if muted {
            theme.muted()
        } else {
            theme.fg(theme.path)
        };

        let mut spans = vec![Span::styled("Search ".to_string(), bold_style)];

        // Search term: either promoted glob or quoted pattern
        if self.is_trivial_pattern()
            && let Some(ref glob) = self.meta.glob
        {
            // Case 1: glob IS the search term — no quotes, string-styled
            spans.push(Span::styled(glob.to_string(), pattern_style));
        } else {
            // Cases 2 & 3: quoted regex pattern
            spans.push(Span::styled(format!("{:?}", self.pattern), pattern_style));

            // Case 2: glob shown as first "in" scope (string-styled, not path)
            if let Some(ref glob) = self.meta.glob {
                spans.push(Span::styled(" in ".to_string(), text_style));
                spans.push(Span::styled(glob.to_string(), pattern_style));
            }
        }

        // Path scope (always after glob if both present).
        // When width-constrained, fish-shorten the path.
        if let Some(ref path) = self.meta.path {
            spans.push(Span::styled(" in ".to_string(), text_style));
            if let Some(w) = width {
                let used: usize = spans
                    .iter()
                    .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                let summary = format!(" {}", self.match_summary());
                // Reserve space for summary; if path can't fit even without it, drop summary.
                let path_budget = w.saturating_sub(used + summary.len());
                let shortened = crate::render::tool_paths::shorten_path(path, path_budget);
                spans.push(Span::styled(shortened, path_style));
            } else {
                spans.push(Span::styled(path.to_string(), path_style));
            }
        }

        // Match summary — always last.
        // When width-constrained, only include if there's room.
        let summary = format!(" {}", self.match_summary());
        if let Some(w) = width {
            let used: usize = spans
                .iter()
                .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            if used + summary.len() <= w {
                spans.push(Span::styled(summary, detail_style));
            }
        } else {
            spans.push(Span::styled(summary, detail_style));
        }

        let line = Line::from(spans);
        if let Some(w) = width {
            crate::render::line_utils::truncate_line(line, w)
        } else {
            line
        }
    }

    /// Textable operand shown in the header (glob when it replaces a trivial pattern).
    fn header_selection_text(&self) -> String {
        if self.is_trivial_pattern()
            && let Some(ref glob) = self.meta.glob
        {
            glob.clone()
        } else {
            self.pattern.clone()
        }
    }

    /// Header line with only the search term span selectable (exclude "Search " prefix).
    ///
    /// Span 0 is always the label; span 1 is the pattern/glob. Later "in path"
    /// and summary spans stay non-selectable so copy yields the search term.
    fn header_block_line(&self, line: Line<'static>) -> BlockLine {
        let term_end = 2.min(line.spans.len()).max(1);
        BlockLine {
            selectable: Selectable::Spans(1..term_end),
            selection_range: Some(TOOL_HEADER_RANGE),
            selection_text: Some(self.header_selection_text()),
            content: line,
            ..Default::default()
        }
    }

    /// Build a single comma-separated metadata line.
    ///
    /// Always present (at minimum shows `mode: pattern`).
    /// Glob is never shown here (always inline in header).
    /// All flags use `key: value` form. Values in primary fg, keys in muted.
    fn metadata_line(&self, theme: &Theme) -> Line<'static> {
        let label_style = theme.muted();
        let value_style = theme.primary();

        let mut parts: Vec<Vec<Span<'static>>> = Vec::new();

        // Mode is always first — grounds the user in what kind of search this is.
        let mode_str = match self.meta.output_mode {
            SearchOutputMode::Content => "pattern",
            SearchOutputMode::FilesWithMatches => "files",
            SearchOutputMode::Count => "count",
        };
        parts.push(vec![
            Span::styled("mode: ", label_style),
            Span::styled(mode_str.to_string(), value_style),
        ]);

        if let Some(ref ft) = self.meta.file_type {
            parts.push(vec![
                Span::styled("type: ", label_style),
                Span::styled(ft.to_string(), value_style),
            ]);
        }
        if self.meta.case_insensitive {
            parts.push(vec![
                Span::styled("case-insensitive: ", label_style),
                Span::styled("true", value_style),
            ]);
        }
        if self.meta.multiline {
            parts.push(vec![
                Span::styled("multiline: ", label_style),
                Span::styled("true", value_style),
            ]);
        }

        let indent = "  ";
        let mut spans: Vec<Span<'static>> = vec![Span::styled(indent.to_string(), label_style)];
        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(", ", label_style));
            }
            spans.extend(part);
        }

        Line::from(spans)
    }
}

impl BlockContent for SearchToolCallBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let tool_cfg = &ctx.appearance.scrollback.blocks.tool;
        let muted_collapsed = ctx.mute_when_collapsed(tool_cfg.muted_collapsed);
        let dim_details = tool_cfg.dim_details;

        match ctx.mode {
            DisplayMode::Collapsed => BlockOutput {
                lines: vec![self.header_block_line(self.header_line(
                    &theme,
                    muted_collapsed,
                    dim_details,
                    Some(ctx.content_width()),
                ))],
            },
            DisplayMode::Truncated | DisplayMode::Expanded => {
                let mut lines: Vec<BlockLine> = vec![self.header_block_line(self.header_line(
                    &theme,
                    false,
                    dim_details,
                    None,
                ))];

                // Metadata line (mode + non-default input fields, comma-separated).
                // Blank line separates header from metadata.
                lines.push(BlockLine::separator(Line::from("")));
                lines.push(BlockLine::separator(self.metadata_line(&theme)));

                let has_results = !self.file_matches.is_empty() || !self.file_paths.is_empty();

                if has_results {
                    // Blank line before results
                    lines.push(Line::from("").into());
                } else if self.match_count == 0 {
                    // No results — show a hint
                    lines.push(Line::from("").into());
                    lines.push(
                        Line::from(Span::styled("  (no results)".to_string(), theme.muted()))
                            .into(),
                    );
                }

                if !self.file_matches.is_empty() {
                    // Line-level matches (content mode).
                    // Each file group is a separate bg_dark block, separated
                    // by a blank line.
                    let indent = "  ";
                    let match_indent = "    ";

                    for (i, file_match) in self.file_matches.iter().enumerate() {
                        if i > 0 {
                            // Blank line between file groups
                            lines.push(Line::from("").into());
                        }

                        // File path line
                        lines.push(
                            BlockLine::from(Line::from(Span::styled(
                                format!("{}{}", indent, file_match.path),
                                theme.fg(theme.path),
                            )))
                            .with_panel_background(theme.bg_dark),
                        );

                        // Match lines: "    42  content..."
                        for m in &file_match.matches {
                            let line_num_str = format!("{:>4}", m.line_number);
                            let content_trimmed = m.content.trim_end();
                            lines.push(
                                BlockLine::from(Line::from(vec![
                                    Span::styled(match_indent.to_string(), theme.primary()),
                                    Span::styled(line_num_str, theme.muted()),
                                    Span::styled("  ".to_string(), theme.primary()),
                                    Span::styled(content_trimmed.to_string(), theme.primary()),
                                ]))
                                .with_panel_background(theme.bg_dark),
                            );
                        }
                    }
                } else if !self.file_paths.is_empty() {
                    // File paths only (files_with_matches mode) OR
                    // count mode (path:N lines).
                    let indent = "  ";
                    let is_count = self.meta.output_mode == SearchOutputMode::Count;

                    for path in &self.file_paths {
                        let line = if is_count {
                            // Count mode: "path:N" — split at last ':',
                            // path part in path color, ":N" in normal fg.
                            if let Some(colon_pos) = path.rfind(':') {
                                let file_part = &path[..colon_pos];
                                let count_part = &path[colon_pos..]; // includes ':'
                                Line::from(vec![
                                    Span::styled(
                                        format!("{indent}{file_part}"),
                                        theme.fg(theme.path),
                                    ),
                                    Span::styled(count_part.to_string(), theme.primary()),
                                ])
                            } else {
                                Line::from(Span::styled(
                                    format!("{indent}{path}"),
                                    theme.fg(theme.path),
                                ))
                            }
                        } else {
                            Line::from(Span::styled(
                                format!("{indent}{path}"),
                                theme.fg(theme.path),
                            ))
                        };

                        lines.push(BlockLine::from(line).with_panel_background(theme.bg_dark));
                    }
                }

                BlockOutput { lines }
            }
        }
    }

    fn accent(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        None // Search blocks never have an accent line
    }

    fn bullet(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        if self.error.is_some() {
            let theme = Theme::current();
            Some(AccentStyle::static_color(theme.accent_error))
        } else {
            None
        }
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        false
    }

    fn background(&self, _ctx: &BlockContext) -> BlockBackground {
        BlockBackground::None
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        // Always foldable (even with no results — expand shows metadata
        // and/or "(no results)" for consistency).
        self.error.is_none()
    }

    fn default_display_mode(&self) -> DisplayMode {
        DisplayMode::Collapsed
    }

    fn next_fold_mode(&self, current: DisplayMode, _is_running: bool) -> DisplayMode {
        match current {
            DisplayMode::Collapsed => DisplayMode::Expanded,
            _ => DisplayMode::Collapsed,
        }
    }
}
