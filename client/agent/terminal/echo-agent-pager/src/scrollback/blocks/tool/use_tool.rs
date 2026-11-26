//! UseToolCallBlock — MCP integration tool dispatch.

use echo_agent_workspace::permission::{MCP_TOOL_NAME_DELIMITER, mcp_titleize_segment};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span, Text};

use crate::appearance::AppearanceConfig;
use crate::render::line_utils::truncate_str;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{
    AccentStyle, BlockBackground, BlockContext, BlockLine, BlockOutput, DisplayMode,
};
use crate::theme::Theme;

const MAX_INLINE_LINES: usize = 10;
const TRUNCATED_INLINE_LINES: usize = 3;

/// Use tool call — dispatching to an MCP integration tool.
#[derive(Debug, Clone)]
pub struct UseToolCallBlock {
    /// The qualified tool name (e.g. "linear__save_issue").
    pub tool_name: String,
    /// Input arguments as key-value pairs (extracted from tool_input JSON).
    pub input_args: Vec<(String, String)>,
    /// Output text from the dispatched tool.
    pub output: Option<String>,
    /// Error message if the tool call failed.
    pub error: Option<String>,
    /// When the tool started running.
    pub started_at: Option<std::time::Instant>,
    /// Elapsed time in ms after completion.
    pub elapsed_ms: Option<i64>,
}

impl UseToolCallBlock {
    pub fn new(tool_name: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            input_args: Vec::new(),
            output: None,
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
        let mut out = format!("tool: {}\n", self.tool_name);
        for (k, v) in &self.input_args {
            out.push_str(&format!("{k}: {v}\n"));
        }
        out.push('\n');
        out.push_str(self.output.as_deref().unwrap_or("(no output)"));
        out
    }

    /// Split `tool_name` on the (validated-unambiguous)
    /// `MCP_TOOL_NAME_DELIMITER` and title-case each segment. Returns
    /// `(server_title, action_title)` for qualified names, or
    /// `("", titleized_tool_name)` for unqualified ones (which fall
    /// through to a single-span render in `header_line`).
    fn split_name(&self) -> (String, String) {
        match self.tool_name.split_once(MCP_TOOL_NAME_DELIMITER) {
            Some((server, action)) => (mcp_titleize_segment(server), mcp_titleize_segment(action)),
            None => (String::new(), mcp_titleize_segment(&self.tool_name)),
        }
    }

    /// Render the header line: **Server** `Action`
    fn header_line(&self, theme: &Theme, muted: bool, max_width: Option<usize>) -> Line<'static> {
        let text_style = if muted {
            theme.muted()
        } else {
            theme.primary()
        };
        let bold_style = text_style.add_modifier(Modifier::BOLD);
        let action_style = if muted {
            theme.muted()
        } else {
            theme.fg(theme.command)
        };

        let (server, action) = self.split_name();

        if server.is_empty() {
            let display = match max_width {
                Some(w) => truncate_str(&action, w),
                None => action,
            };
            return Line::from(vec![Span::styled(display, bold_style)]);
        }

        let prefix = format!("{server} ");

        match max_width {
            Some(w) => {
                let budget = w.saturating_sub(prefix.len());
                let display_action = truncate_str(&action, budget);
                Line::from(vec![
                    Span::styled(prefix, bold_style),
                    Span::styled(display_action, action_style),
                ])
            }
            None => Line::from(vec![
                Span::styled(prefix, bold_style),
                Span::styled(action, action_style),
            ]),
        }
    }
}

impl BlockContent for UseToolCallBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let muted_collapsed =
            ctx.mute_when_collapsed(ctx.appearance.scrollback.blocks.tool.muted_collapsed);

        match ctx.mode {
            DisplayMode::Collapsed => BlockOutput {
                lines: vec![
                    self.header_line(&theme, muted_collapsed, Some(ctx.content_width()))
                        .into(),
                ],
            },
            DisplayMode::Truncated | DisplayMode::Expanded => {
                let header = self.header_line(&theme, false, None);
                let wrapped = crate::render::wrapping::wrap_header_flush(
                    header,
                    ctx.width as usize,
                    ctx.bullet_indent(),
                );
                let mut lines: Vec<BlockLine> = wrapped.into_iter().map(BlockLine::from).collect();

                // Input arguments
                if !self.input_args.is_empty() {
                    lines.push(Line::from("").into());
                    for (key, val) in &self.input_args {
                        lines.push(BlockLine::styled(Line::from(vec![
                            Span::styled(format!("  {key}: "), theme.muted()),
                            Span::styled(val.clone(), theme.primary()),
                        ])));
                    }
                }

                let max_inline = if ctx.mode == DisplayMode::Truncated {
                    TRUNCATED_INLINE_LINES
                } else {
                    MAX_INLINE_LINES
                };
                if let Some(ref output) = self.output {
                    lines.push(Line::from("").into());
                    lines
                        .push(BlockLine::from(Line::from("")).with_panel_background(theme.bg_dark));

                    let indent = "  ";
                    let content_lines: Vec<&str> = output.lines().collect();

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

                    lines
                        .push(BlockLine::from(Line::from("")).with_panel_background(theme.bg_dark));
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
        !self.input_args.is_empty() || self.output.is_some() || self.error.is_some()
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

    fn rendered_text(block: &UseToolCallBlock, mode: DisplayMode) -> String {
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
    fn truncated_caps_inline_output_tighter_than_expanded() {
        let mut block = UseToolCallBlock::new("linear__list_issues");
        let content: Vec<String> = (1..=12).map(|i| format!("l{i:02} row")).collect();
        block.output = Some(content.join("\n"));

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
