//! WebSearchToolCallBlock — web search with citations preview.

use std::collections::HashSet;

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

/// Max number of domain names shown in the sources summary line.
const MAX_INLINE_SOURCES: usize = 3;

/// Web search tool call — searching the web and returning markdown results.
#[derive(Debug, Clone)]
pub struct WebSearchToolCallBlock {
    /// The search query.
    pub query: String,
    /// Markdown-formatted search results.
    pub content: Option<String>,
    /// Source URLs from the search.
    pub citations: Vec<String>,
    /// Error message if the tool call failed (None = success).
    pub error: Option<String>,
    /// When the tool started running.
    pub started_at: Option<std::time::Instant>,
    /// Elapsed time in ms after completion.
    pub elapsed_ms: Option<i64>,
    /// Header label override (default: "Web Search ").
    pub label: Option<String>,
    /// True for X search (backend); suppresses the content body since
    /// structured post results are not exposed to the TUI client.
    pub is_x_search: bool,
}

impl WebSearchToolCallBlock {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            content: None,
            citations: Vec::new(),
            error: None,
            started_at: None,
            elapsed_ms: None,
            label: None,
            is_x_search: false,
        }
    }

    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    pub fn copy_text(&self) -> String {
        self.content.as_deref().unwrap_or_default().to_owned()
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
        self.elapsed_ms.or_else(|| {
            self.started_at
                .map(|start| start.elapsed().as_millis() as i64)
        })
    }

    /// Render the header line: **Web Search** `query` `(N sources)`
    ///
    /// In collapsed mode (`max_width` is `Some`), reserves space for the source
    /// count suffix and truncates the query to fit — so the suffix is always
    /// visible. In expanded mode (`None`), renders the full query with no suffix.
    fn header_line(&self, theme: &Theme, muted: bool, max_width: Option<usize>) -> Line<'static> {
        let text_style = if muted {
            theme.muted()
        } else {
            theme.primary()
        };
        let bold_style = text_style.add_modifier(Modifier::BOLD);
        let query_style = if muted {
            theme.muted()
        } else {
            theme.fg(theme.command)
        };

        let prefix = self.label.as_deref().unwrap_or("Web Search ").to_owned();

        match max_width {
            Some(w) => {
                // Collapsed shows deduplicated domain count as "sites".
                // The fullscreen footer shows raw citation count as "Sources".
                let site_count = self.unique_domains().len();
                let suffix = if site_count > 0 {
                    let s = if site_count == 1 { "" } else { "s" };
                    format!(" ({site_count} site{s})")
                } else {
                    String::new()
                };

                // Only show suffix if prefix + suffix fit within width.
                // Otherwise drop it to avoid overflow on narrow terminals.
                let suffix_fits = prefix.len() + suffix.len() < w;
                let effective_suffix = if suffix_fits { &suffix } else { "" };

                let query_budget = w
                    .saturating_sub(prefix.len())
                    .saturating_sub(effective_suffix.len());
                let display_query = truncate_str(&self.query, query_budget);

                let mut spans = vec![
                    Span::styled(prefix, bold_style),
                    Span::styled(display_query, query_style),
                ];
                if !effective_suffix.is_empty() {
                    spans.push(Span::styled(effective_suffix.to_string(), theme.dim()));
                }
                Line::from(spans)
            }
            None => {
                // Expanded: full query, no suffix.
                Line::from(vec![
                    Span::styled(prefix, bold_style),
                    Span::styled(self.query.clone(), query_style),
                ])
            }
        }
    }

    /// Header line with only the query span selectable (exclude label prefix/suffix).
    fn header_block_line(&self, line: Line<'static>) -> BlockLine {
        // Spans: [prefix, query, optional_suffix] — only the query (index 1).
        let query_end = 2.min(line.spans.len()).max(1);
        BlockLine {
            selectable: Selectable::Spans(1..query_end),
            selection_range: Some(TOOL_HEADER_RANGE),
            selection_text: Some(self.query.clone()),
            content: line,
            ..Default::default()
        }
    }

    /// Unique domain names from citations, deduplicated and order-preserved.
    fn unique_domains(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.citations
            .iter()
            .filter_map(|url| extract_domain(url))
            .filter(|d| seen.insert(d.clone()))
            .collect()
    }

    /// Build the sources summary line from citations.
    ///
    /// Extracts domain names from URLs and renders a compact one-liner:
    /// `Sources: stripe.com, react.dev, stackoverflow.com (+2 more)`
    fn sources_line(&self, theme: &Theme) -> Option<Line<'static>> {
        let unique = self.unique_domains();
        if unique.is_empty() {
            return None;
        }

        let label_style = theme.muted();
        let value_style = theme.primary();

        let mut spans: Vec<Span<'static>> = vec![Span::styled("  Sources: ", label_style)];

        let shown = unique.len().min(MAX_INLINE_SOURCES);
        for (i, domain) in unique.iter().take(shown).enumerate() {
            if i > 0 {
                spans.push(Span::styled(", ", label_style));
            }
            spans.push(Span::styled(domain.clone(), value_style));
        }

        let remaining = unique.len().saturating_sub(MAX_INLINE_SOURCES);
        if remaining > 0 {
            spans.push(Span::styled(format!(" (+{remaining} more)"), label_style));
        }

        Some(Line::from(spans))
    }
}

/// Extract the host/domain from a URL for display purposes.
fn extract_domain(raw: &str) -> Option<String> {
    url::Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_owned()))
}

impl BlockContent for WebSearchToolCallBlock {
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
            DisplayMode::Truncated | DisplayMode::Expanded => {
                let header = self.header_line(&theme, false, None);
                let wrapped = crate::render::wrapping::wrap_header_flush(
                    header,
                    ctx.width as usize,
                    ctx.bullet_indent(),
                );
                // Header lines: label prefix excluded, query selectable.
                let mut lines: Vec<BlockLine> = wrapped
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let total = line.spans.len();
                        BlockLine {
                            selectable: Selectable::Spans(1..total),
                            selection_range: Some(TOOL_HEADER_RANGE),
                            selection_text: if i == 0 {
                                Some(self.query.clone())
                            } else {
                                None
                            },
                            joiner: if i == 0 { None } else { Some(" ".to_string()) },
                            content: line,
                            ..Default::default()
                        }
                    })
                    .collect();

                let max_inline = if ctx.mode == DisplayMode::Truncated {
                    TRUNCATED_INLINE_LINES
                } else {
                    MAX_INLINE_LINES
                };
                if let Some(ref content) = self.content {
                    lines.push(BlockLine::separator(Line::from("")));

                    // Top padding inside the content box.
                    lines
                        .push(BlockLine::from(Line::from("")).with_panel_background(theme.bg_dark));

                    let indent = "  ";
                    let content_lines: Vec<&str> = content.lines().collect();

                    for (i, line) in content_lines.iter().enumerate() {
                        if i >= max_inline {
                            let remaining = content_lines.len() - max_inline;
                            lines.push(
                                BlockLine::from(Line::from(Span::styled(
                                    format!(
                                        "{indent}... ({remaining} more lines, press Enter to view)",
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
                } else if let Some(ref err) = self.error {
                    lines.push(Line::from("").into());
                    lines.push(
                        Line::from(Span::styled(
                            format!("  {err}"),
                            theme.fg(theme.accent_error),
                        ))
                        .into(),
                    );
                } else if !self.is_x_search {
                    lines.push(Line::from("").into());
                    lines.push(Line::from(Span::styled("  (no content)", theme.muted())).into());
                }

                // Sources summary line (after content, matching fullscreen order).
                if let Some(sources) = self.sources_line(&theme) {
                    lines.push(Line::from("").into());
                    lines.push(sources.into());
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
        self.error.is_none() && self.content.is_some() && !self.is_x_search
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

    fn rendered_text(block: &WebSearchToolCallBlock, mode: DisplayMode) -> String {
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
        let mut block = WebSearchToolCallBlock::new("rust async traits");
        let content: Vec<String> = (1..=12).map(|i| format!("l{i:02} result")).collect();
        block.content = Some(content.join("\n"));

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
