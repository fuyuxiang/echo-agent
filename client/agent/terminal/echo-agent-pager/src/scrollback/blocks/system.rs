//! SystemMessageBlock - displays system messages.

use ratatui::text::{Line, Span};

use crate::appearance::AppearanceConfig;
use crate::render::wrapping::word_wrap_lines;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{AccentStyle, BlockContext, BlockLine, BlockOutput, Selectable};
use crate::theme::Theme;

/// Block displaying a system message.
#[derive(Debug, Clone)]
pub struct SystemMessageBlock {
    /// The message text.
    pub text: String,
}

impl SystemMessageBlock {
    /// Create a new system message block.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl BlockContent for SystemMessageBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let style = theme.muted();

        let styled_lines: Vec<Line<'static>> = self
            .text
            .lines()
            .map(|line| Line::from(Span::styled(line.to_string(), style)))
            .collect();
        let wrapped = word_wrap_lines(styled_lines, ctx.width as usize);
        let all_lines: Vec<BlockLine> = wrapped
            .into_iter()
            .map(|line| BlockLine::styled(line).with_selection_range(Some(0)))
            .collect();

        // Apply max_lines budget if set
        let lines = if let Some(max) = ctx.max_lines {
            let max = max as usize;
            if all_lines.len() > max && max > 0 {
                let take_count = if max > 1 { max - 1 } else { 1 };
                let mut truncated: Vec<BlockLine> =
                    all_lines.into_iter().take(take_count).collect();
                if let Some(last) = truncated.last_mut() {
                    let content_end = last.content.spans.len();
                    last.content
                        .spans
                        .push(Span::styled(" \u{2026}".to_string(), style));
                    last.selectable = Selectable::Spans(0..content_end);
                }
                truncated
            } else {
                all_lines
            }
        } else {
            all_lines
        };

        if lines.is_empty() {
            BlockOutput {
                lines: vec![BlockLine::styled(Line::from("")).with_selection_range(Some(0))],
            }
        } else {
            BlockOutput { lines }
        }
    }

    fn accent(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        None // System messages have no accent
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        false // System messages are compact
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        false // System messages are short
    }

    fn is_selectable(&self) -> bool {
        false // System messages are not navigable
    }

    fn is_groupable(&self) -> bool {
        true
    }
}
