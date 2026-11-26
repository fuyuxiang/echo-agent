//! BtwBlock — scrollback entry for /btw side-question responses.
//!
//! Renders with a golden accent line. Collapsed (default) shows a
//! single `/btw <question>` header line; expanded shows the full
//! markdown response below the header.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{AccentStyle, BlockContext, BlockLine, BlockOutput, DisplayMode};
use crate::theme::Theme;

use super::markdown_content::MarkdownContent;
use crate::appearance::AppearanceConfig;

/// Block displaying a /btw side-question and its response.
#[derive(Debug, Clone)]
pub struct BtwBlock {
    /// The original question text.
    pub question: String,
    /// Rendered response content (markdown).
    content: MarkdownContent,
}

impl BtwBlock {
    /// Create a btw block from the question and response text.
    pub fn new(question: impl Into<String>, response: impl Into<String>) -> Self {
        Self {
            question: question.into(),
            content: MarkdownContent::new(response),
        }
    }

    /// Access the underlying markdown content.
    pub fn content(&self) -> &MarkdownContent {
        &self.content
    }
}

impl BlockContent for BtwBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let is_collapsed = ctx.mode == DisplayMode::Collapsed;
        let tool_cfg = &ctx.appearance.scrollback.blocks.tool;
        let is_muted = tool_cfg.muted_collapsed && is_collapsed;

        // Header: "/btw <question>"
        let header_style = if is_muted {
            theme.muted().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme.accent_plan)
                .add_modifier(Modifier::BOLD)
        };
        let header = Line::from(Span::styled(
            format!("/btw {}", self.question),
            header_style,
        ));

        let mut lines = vec![BlockLine::styled(header).with_selection_range(Some(0))];

        // Collapsed: header only. Expanded: header + separator + markdown body.
        if !is_collapsed {
            lines.push(BlockLine::separator(Line::from("")));
            let body = self.content.output(ctx.width as usize);
            for mut bl in body.lines {
                bl.selection_range = Some(0);
                lines.push(bl);
            }
        }

        BlockOutput { lines }
    }

    fn accent(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        None
    }

    fn has_bullet(&self, _ctx: &BlockContext) -> bool {
        true
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        false
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        true
    }

    fn is_groupable(&self) -> bool {
        true
    }

    fn default_display_mode(&self) -> DisplayMode {
        DisplayMode::Collapsed
    }
}
