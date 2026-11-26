//! ListDirToolCallBlock - lists directory contents.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{
    AccentStyle, BlockBackground, BlockContext, BlockLine, BlockOutput, DisplayMode, Selectable,
};
use crate::theme::Theme;

use super::TOOL_HEADER_RANGE;
use crate::appearance::AppearanceConfig;

/// List directory tool call.
#[derive(Debug, Clone)]
pub struct ListDirToolCallBlock {
    /// Path to the directory.
    pub path: String,
    /// The formatted directory listing output.
    pub output: String,
    /// Error message if the tool call failed (None = success).
    pub error: Option<String>,
    /// When the tool started running (Phase 2: time tracking).
    pub started_at: Option<std::time::Instant>,
    /// Elapsed time in ms after completion (Phase 2: time tracking).
    pub elapsed_ms: Option<i64>,
}

impl ListDirToolCallBlock {
    /// Create a new list_dir block.
    ///
    /// Pre-completed blocks have no meaningful local timing — `started_at`
    /// is `None`. Timing is only set for blocks that enter a running UI
    /// state (via `set_last_running(true)` in `ScrollbackState`).
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            output: String::new(),
            error: None,
            started_at: None,
            elapsed_ms: None,
        }
    }

    /// Set the output.
    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = output.into();
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

    /// Set output (mutable).
    pub fn set_output(&mut self, output: impl Into<String>) {
        self.output = output.into();
    }

    fn collapsed_line(&self, theme: &Theme, muted: bool, width: Option<usize>) -> Line<'static> {
        let text_style = if muted {
            theme.muted()
        } else {
            theme.primary()
        };
        let bold_style = text_style.add_modifier(Modifier::BOLD);
        let path_style = if muted {
            theme.muted()
        } else {
            theme.fg(theme.path)
        };

        let prefix = "List ";
        let entry_count = self.output.lines().filter(|l| !l.trim().is_empty()).count();
        let suffix = if self.error.is_none() && entry_count > 0 {
            let s = if entry_count == 1 { "y" } else { "ies" };
            format!(" ({entry_count} entr{s})")
        } else {
            String::new()
        };
        let suffix_fits = width.is_none_or(|w| prefix.len() + suffix.len() < w);
        let effective_suffix = if suffix_fits { suffix.as_str() } else { "" };

        let path_budget = width
            .map(|w| {
                w.saturating_sub(prefix.len())
                    .saturating_sub(effective_suffix.len())
            })
            .unwrap_or(usize::MAX);
        let path = crate::render::tool_paths::shorten_path(&self.path, path_budget);

        let mut spans = vec![
            Span::styled(prefix, bold_style),
            Span::styled(path, path_style),
        ];
        if !effective_suffix.is_empty() {
            spans.push(Span::styled(effective_suffix.to_string(), theme.muted()));
        }
        Line::from(spans)
    }

    /// Header line with only the path span selectable (exclude "List " prefix).
    fn header_block_line(&self, line: Line<'static>) -> BlockLine {
        let path_end = 2.min(line.spans.len()).max(1);
        BlockLine {
            selectable: Selectable::Spans(1..path_end),
            selection_range: Some(TOOL_HEADER_RANGE),
            selection_text: Some(self.path.clone()),
            content: line,
            ..Default::default()
        }
    }
}

impl BlockContent for ListDirToolCallBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let muted_collapsed =
            ctx.mute_when_collapsed(ctx.appearance.scrollback.blocks.tool.muted_collapsed);
        let terminal_bg = ctx.appearance.scrollback.blocks.list_dir.terminal_bg;

        match ctx.mode {
            DisplayMode::Collapsed => BlockOutput {
                lines: vec![self.header_block_line(self.collapsed_line(
                    &theme,
                    muted_collapsed,
                    Some(ctx.content_width()),
                ))],
            },
            DisplayMode::Truncated | DisplayMode::Expanded => {
                let mut lines: Vec<BlockLine> =
                    vec![self.header_block_line(self.collapsed_line(&theme, false, None))];

                if !self.output.is_empty() {
                    lines.push(BlockLine::separator(Line::from("")));

                    for rl in crate::render::terminal_output::render_terminal_lines(
                        &self.output,
                        theme.primary(),
                    ) {
                        // Indent output by 2 spaces
                        let mut spans = vec![Span::styled("  ".to_string(), theme.primary())];
                        spans.extend(rl.line.spans);
                        let mut block_line: BlockLine = Line::from(spans).into();
                        if terminal_bg {
                            block_line = block_line.with_panel_background(theme.bg_dark);
                        }
                        lines.push(block_line);
                    }
                }

                BlockOutput { lines }
            }
        }
    }

    fn accent(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        None // ListDir blocks never have an accent line
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
        // Not foldable if failed
        if self.error.is_some() {
            return false;
        }
        !self.output.is_empty()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::types::BlockContext;

    fn ctx() -> BlockContext {
        BlockContext {
            width: 80,
            mode: DisplayMode::Collapsed,
            is_running: false,
            raw: false,
            max_lines: None,
            appearance: Default::default(),
            is_selected: false,
            cwd: None,
        }
    }

    fn header_text(block: &ListDirToolCallBlock) -> String {
        block.output(&ctx()).lines[0]
            .content
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn collapsed_header_shows_entry_count() {
        let block = ListDirToolCallBlock::new("src").with_output("a.rs\nb.rs\nsub/\n");
        assert_eq!(header_text(&block), "List src (3 entries)");

        let single = ListDirToolCallBlock::new("src").with_output("lonely.rs\n");
        assert_eq!(header_text(&single), "List src (1 entry)");
    }

    #[test]
    fn collapsed_header_omits_count_when_empty_or_failed() {
        let empty = ListDirToolCallBlock::new("src");
        assert_eq!(header_text(&empty), "List src");

        let failed = ListDirToolCallBlock::new("gone").with_error("no such directory");
        assert_eq!(header_text(&failed), "List gone");
    }
}
