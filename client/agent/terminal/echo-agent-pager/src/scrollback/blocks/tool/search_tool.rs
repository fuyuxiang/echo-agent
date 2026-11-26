//! SearchToolCallBlock — integration tool discovery results.

use echo_agent_workspace::permission::mcp_titleize_segment;
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

/// A tool discovered via search_tool.
#[derive(Debug, Clone)]
pub struct DiscoveredTool {
    pub name: String,
    pub server: String,
    pub description: String,
    pub score: f64,
}

/// Search tool call — discovering MCP integration tools by keyword.
#[derive(Debug, Clone)]
pub struct SearchToolCallBlock {
    /// The search query.
    pub query: String,
    /// Limit parameter from the input (None = default 8).
    pub limit: Option<u8>,
    /// Number of results found.
    pub result_count: usize,
    /// Discovered tools (parsed from output).
    pub results: Vec<DiscoveredTool>,
    /// Raw output content (full JSON) for the fullscreen viewer.
    pub content: Option<String>,
    /// Error message if the tool call failed.
    pub error: Option<String>,
    /// When the tool started running.
    pub started_at: Option<std::time::Instant>,
    /// Elapsed time in ms after completion.
    pub elapsed_ms: Option<i64>,
}

pub fn discovered_tool_action(tool: &DiscoveredTool) -> &str {
    tool.name
        .strip_prefix(&tool.server)
        .and_then(|rest| rest.strip_prefix("__"))
        .unwrap_or(&tool.name)
}

impl SearchToolCallBlock {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: None,
            result_count: 0,
            results: Vec::new(),
            content: None,
            error: None,
            started_at: None,
            elapsed_ms: None,
        }
    }

    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    pub fn is_success(&self) -> bool {
        self.error.is_none()
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

    pub fn copy_text(&self) -> String {
        let mut out = format!("query: {}\n", self.query);
        if let Some(limit) = self.limit {
            out.push_str(&format!("limit: {limit}\n"));
        }
        let s = if self.result_count == 1 { "" } else { "s" };
        out.push_str(&format!("{} result{s}\n", self.result_count));

        for (i, tool) in self.results.iter().enumerate() {
            out.push('\n');
            let action = mcp_titleize_segment(discovered_tool_action(tool));
            let server = mcp_titleize_segment(&tool.server);
            out.push_str(&format!("{}. {}  {}\n", i + 1, action, server));
            if !tool.description.is_empty() {
                out.push_str(&format!("   {}\n", tool.description));
            }
        }
        out
    }

    /// Render the header line: **Search Tools** `query` `(N results)`
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

        let prefix = "Search Tools ";

        match max_width {
            Some(w) => {
                let s = if self.result_count == 1 { "" } else { "s" };
                let suffix = format!(" ({} result{s})", self.result_count);

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
            None => Line::from(vec![
                Span::styled(prefix, bold_style),
                Span::styled(self.query.clone(), query_style),
            ]),
        }
    }

    /// Header line with only the query span selectable (exclude label/suffix).
    fn header_block_line(&self, line: Line<'static>) -> BlockLine {
        let query_end = 2.min(line.spans.len()).max(1);
        BlockLine {
            selectable: Selectable::Spans(1..query_end),
            selection_range: Some(TOOL_HEADER_RANGE),
            selection_text: Some(self.query.clone()),
            content: line,
            ..Default::default()
        }
    }
}

impl BlockContent for SearchToolCallBlock {
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

                if !self.results.is_empty() {
                    lines.push(BlockLine::separator(Line::from("")));

                    for (i, tool) in self.results.iter().enumerate() {
                        let idx_span = Span::styled(format!("  {}. ", i + 1), theme.muted());

                        // Strip the trusted server prefix from tool_name and
                        // title-case both halves; show the action bold and
                        // the server name ghosted on the right.
                        let action = mcp_titleize_segment(discovered_tool_action(tool));
                        let server_label = mcp_titleize_segment(&tool.server);

                        let name_span =
                            Span::styled(action, theme.primary().add_modifier(Modifier::BOLD));

                        let mut spans = vec![idx_span, name_span];
                        if !server_label.is_empty() {
                            spans.push(Span::styled(format!("  {server_label}"), theme.dim()));
                        }
                        lines.push(BlockLine::styled(Line::from(spans)));
                    }
                } else if self.error.is_none() {
                    lines.push(Line::from("").into());
                    lines.push(
                        Line::from(Span::styled("  (no results found)", theme.muted())).into(),
                    );
                }

                if let Some(ref err) = self.error {
                    lines.push(Line::from("").into());
                    lines.push(
                        Line::from(Span::styled(
                            format!("  {err}"),
                            theme.fg(theme.accent_error),
                        ))
                        .into(),
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
        self.error.is_none() && !self.results.is_empty()
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

    #[test]
    fn discovered_tool_action_strips_local_mcp_prefix() {
        let tool = DiscoveredTool {
            name: "linear__save_issue".into(),
            server: "linear".into(),
            description: String::new(),
            score: 1.0,
        };
        assert_eq!(discovered_tool_action(&tool), "save_issue");
    }

    #[test]
    fn discovered_tool_action_keeps_gateway_flat_name() {
        let tool = DiscoveredTool {
            name: "google_calendar_search".into(),
            server: "Google Calendar".into(),
            description: String::new(),
            score: 1.0,
        };
        assert_eq!(discovered_tool_action(&tool), "google_calendar_search");
    }

    #[test]
    fn discovered_tool_action_strips_gateway_mcp_prefix() {
        let tool = DiscoveredTool {
            name: "google_calendar__search".into(),
            server: "google_calendar".into(),
            description: String::new(),
            score: 1.0,
        };
        assert_eq!(discovered_tool_action(&tool), "search");
    }
}
