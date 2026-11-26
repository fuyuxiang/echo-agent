//! WebFetchToolCallBlock — URL fetch with content preview.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span, Text};

use super::TOOL_HEADER_RANGE;
use crate::appearance::AppearanceConfig;
use crate::render::line_utils::truncate_str;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{
    AccentStyle, BlockBackground, BlockContext, BlockLine, BlockOutput, DisplayMode, Selectable,
};
use crate::theme::Theme;

const MAX_INLINE_LINES: usize = 10;
const TRUNCATED_INLINE_LINES: usize = 3;

/// Web fetch tool call — fetching a URL and returning markdown content.
#[derive(Debug, Clone)]
pub struct WebFetchToolCallBlock {
    /// The fetched URL.
    pub url: String,
    /// HTTP status code (e.g. 200, 404).
    /// `Option` because the block exists pre-completion (pending/running state)
    /// before any response data arrives.
    pub status_code: Option<u16>,
    /// Content type (e.g. "markdown", "text/plain").
    pub content_type: Option<String>,
    /// Content size in bytes.
    pub bytes: Option<usize>,
    /// Error message if the tool call failed (None = success).
    pub error: Option<String>,
    /// Fetched content (markdown or raw text).
    pub output: Option<String>,
    /// When the tool started running.
    pub started_at: Option<std::time::Instant>,
    /// Elapsed time in ms after completion.
    pub elapsed_ms: Option<i64>,
}

impl WebFetchToolCallBlock {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            status_code: None,
            content_type: None,
            bytes: None,
            error: None,
            output: None,
            started_at: None,
            elapsed_ms: None,
        }
    }

    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = Some(output.into());
        self
    }

    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    pub fn copy_text(&self) -> String {
        self.output.clone().unwrap_or_default()
    }

    pub fn set_error(&mut self, error: Option<String>) {
        if self.elapsed_ms.is_none()
            && let Some(start) = self.started_at
        {
            self.elapsed_ms = Some(start.elapsed().as_millis() as i64);
        }
        self.error = error;
    }

    pub fn finish(&mut self) {
        if self.elapsed_ms.is_some() {
            return;
        }
        if let Some(start) = self.started_at {
            self.elapsed_ms = Some(start.elapsed().as_millis() as i64);
        }
    }

    pub fn elapsed_ms(&self) -> Option<i64> {
        match self.elapsed_ms {
            Some(ms) => Some(ms),
            None => self
                .started_at
                .map(|start| start.elapsed().as_millis() as i64),
        }
    }

    /// Render the header line: **Fetch** `url`
    ///
    /// When `max_width` is `Some`, the URL is truncated with ellipsis to fit.
    /// When `None`, the full URL is rendered (for expanded view / fullscreen).
    fn header_line(&self, theme: &Theme, muted: bool, max_width: Option<usize>) -> Line<'static> {
        let text_style = if muted {
            theme.muted()
        } else {
            theme.primary()
        };
        let bold_style = text_style.add_modifier(Modifier::BOLD);
        let url_style = if muted {
            theme.muted()
        } else {
            theme.fg(theme.command)
        };

        let prefix = "Fetch ";
        let display_url = match max_width {
            Some(w) => truncate_str(&self.url, w.saturating_sub(prefix.len())),
            None => self.url.clone(),
        };

        Line::from(vec![
            Span::styled(prefix, bold_style),
            Span::styled(display_url, url_style),
        ])
    }

    /// Header line with only the URL span selectable (exclude "Fetch " prefix).
    fn header_block_line(&self, line: Line<'static>) -> BlockLine {
        let url_end = 2.min(line.spans.len()).max(1);
        BlockLine {
            selectable: Selectable::Spans(1..url_end),
            selection_range: Some(TOOL_HEADER_RANGE),
            selection_text: Some(self.url.clone()),
            content: line,
            ..Default::default()
        }
    }

    /// Build the metadata line: status, content_type, size.
    fn metadata_line(&self, theme: &Theme) -> Option<Line<'static>> {
        let label_style = theme.muted();
        let value_style = theme.primary();

        let mut parts: Vec<Vec<Span<'static>>> = Vec::new();

        if let Some(code) = self.status_code {
            parts.push(vec![
                Span::styled("status: ", label_style),
                Span::styled(code.to_string(), value_style),
            ]);
        }
        if let Some(ref ct) = self.content_type {
            parts.push(vec![
                Span::styled("content_type: ", label_style),
                Span::styled(ct.clone(), value_style),
            ]);
        }
        if let Some(bytes) = self.bytes {
            parts.push(vec![
                Span::styled("size: ", label_style),
                Span::styled(crate::util::format_bytes(bytes as u64), value_style),
            ]);
        }

        if parts.is_empty() {
            return None;
        }

        let indent = "  ";
        let mut spans: Vec<Span<'static>> = vec![Span::styled(indent.to_owned(), label_style)];
        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(", ", label_style));
            }
            spans.extend(part);
        }

        Some(Line::from(spans))
    }
}

impl BlockContent for WebFetchToolCallBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let muted_collapsed =
            ctx.mute_when_collapsed(ctx.appearance.scrollback.blocks.tool.muted_collapsed);

        match ctx.mode {
            DisplayMode::Collapsed => BlockOutput {
                lines: vec![self.header_block_line(self.header_line(
                    &theme,
                    muted_collapsed,
                    Some(ctx.content_width()),
                ))],
            },
            // Fetch completes in one shot (no streaming), so Truncated
            // is never visible in practice. Treat it the same as Expanded
            // to always show the full content the model saw.
            DisplayMode::Truncated | DisplayMode::Expanded => {
                let header = self.header_line(&theme, false, None);
                let wrapped = crate::render::wrapping::wrap_header_flush(
                    header,
                    ctx.width as usize,
                    ctx.bullet_indent(),
                );
                // Header lines: "Fetch " prefix excluded, URL selectable.
                let mut lines: Vec<BlockLine> = wrapped
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let total = line.spans.len();
                        BlockLine {
                            selectable: Selectable::Spans(1..total),
                            selection_range: Some(TOOL_HEADER_RANGE),
                            selection_text: if i == 0 { Some(self.url.clone()) } else { None },
                            joiner: if i == 0 { None } else { Some(" ".to_string()) },
                            content: line,
                            ..Default::default()
                        }
                    })
                    .collect();

                // Metadata line (status, content_type, size).
                if let Some(meta) = self.metadata_line(&theme) {
                    lines.push(BlockLine::separator(Line::from("")));
                    lines.push(BlockLine::separator(meta));
                }

                let max_inline = if ctx.mode == DisplayMode::Truncated {
                    TRUNCATED_INLINE_LINES
                } else {
                    MAX_INLINE_LINES
                };
                if let Some(ref output) = self.output {
                    lines.push(Line::from("").into());

                    // Top padding inside the content box.
                    lines
                        .push(BlockLine::from(Line::from("")).with_panel_background(theme.bg_dark));

                    let indent = "  ";
                    let total_lines = output.lines().count();

                    for (i, line) in output.lines().enumerate() {
                        if i >= max_inline {
                            lines.push(
                                BlockLine::from(Line::from(Span::styled(
                                    format!(
                                        "{indent}... ({} more lines, press Enter to view)",
                                        total_lines - max_inline
                                    ),
                                    theme.dim(),
                                )))
                                .with_panel_background(theme.bg_dark),
                            );
                            break;
                        }
                        lines.push(
                            BlockLine::from(Line::from(Span::styled(
                                format!("{indent}{line}"),
                                theme.primary(),
                            )))
                            .with_panel_background(theme.bg_dark),
                        );
                    }

                    // Bottom padding inside the content box.
                    lines
                        .push(BlockLine::from(Line::from("")).with_panel_background(theme.bg_dark));
                } else if self.error.is_none() {
                    lines.push(Line::from("").into());
                    lines.push(
                        Line::from(Span::styled("  (no content)".to_owned(), theme.muted())).into(),
                    );
                }

                BlockOutput { lines }
            }
        }
    }

    fn accent(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        if ctx.mode == DisplayMode::Collapsed {
            return None;
        }
        let theme = Theme::current();
        if self.error.is_some() {
            Some(AccentStyle::static_color(theme.accent_error))
        } else if ctx.is_running {
            Some(AccentStyle::animated(theme.accent_running))
        } else {
            Some(AccentStyle::static_color(theme.accent_tool))
        }
    }

    fn bullet(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        if self.error.is_some() {
            let theme = Theme::current();
            Some(AccentStyle::static_color(theme.accent_error))
        } else if ctx.mode == DisplayMode::Collapsed {
            None
        } else {
            self.accent(ctx)
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
        self.error.is_none() && self.output.is_some()
    }

    fn default_display_mode(&self) -> DisplayMode {
        DisplayMode::Collapsed
    }

    // No special running-state handling: fetch completes in one shot (no streaming).
    fn next_fold_mode(&self, current: DisplayMode, _is_running: bool) -> DisplayMode {
        match current {
            DisplayMode::Collapsed => DisplayMode::Expanded,
            _ => DisplayMode::Collapsed,
        }
    }

    fn preamble(&self, _ctx: &BlockContext) -> Option<Text<'static>> {
        let theme = Theme::current();
        Some(Text::from(vec![self.header_line(&theme, false, None)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::types::BlockContext;

    fn ctx(mode: DisplayMode) -> BlockContext {
        BlockContext {
            width: 80,
            mode,
            is_running: false,
            raw: false,
            max_lines: None,
            appearance: Default::default(),
            is_selected: false,
            cwd: None,
        }
    }

    fn rendered_text(block: &WebFetchToolCallBlock, mode: DisplayMode) -> String {
        block
            .output(&ctx(mode))
            .lines
            .iter()
            .map(|l| {
                l.content
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn truncated_caps_inline_content_tighter_than_expanded() {
        let content: Vec<String> = (1..=12).map(|i| format!("l{i:02} body")).collect();
        let block =
            WebFetchToolCallBlock::new("https://example.com").with_output(content.join("\n"));

        let truncated = rendered_text(&block, DisplayMode::Truncated);
        assert!(truncated.contains("l03"), "truncated:\n{truncated}");
        assert!(!truncated.contains("l04"), "truncated:\n{truncated}");
        assert!(
            truncated.contains("(9 more lines"),
            "truncated:\n{truncated}"
        );

        let expanded = rendered_text(&block, DisplayMode::Expanded);
        assert!(expanded.contains("l10"), "expanded:\n{expanded}");
        assert!(!expanded.contains("l11"), "expanded:\n{expanded}");
        assert!(expanded.contains("(2 more lines"), "expanded:\n{expanded}");
    }
}
