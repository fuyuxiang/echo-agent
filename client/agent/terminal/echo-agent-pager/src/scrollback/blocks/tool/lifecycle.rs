//! LifecycleEventBlock — standalone block for lifecycle hook events
//! (e.g. `user_prompt_submit`, `session_start`, `session_end`).
//!
//! These are rendered like tool call blocks but are *not* real tool calls.
//! Having a dedicated variant lets `last_tool_call_entry_id()` skip them
//! so that tool-associated hooks (pre/post_tool_use) don't misattach.

use ratatui::text::{Line, Span};

use crate::appearance::AppearanceConfig;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{AccentStyle, BlockContext, BlockOutput, DisplayMode};
use crate::theme::Theme;

/// Block representing a lifecycle hook event.
#[derive(Debug, Clone)]
pub struct LifecycleEventBlock {
    /// Event name (e.g. `user_prompt_submit`).
    pub name: String,
}

impl LifecycleEventBlock {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl BlockContent for LifecycleEventBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let muted_collapsed =
            ctx.mute_when_collapsed(ctx.appearance.scrollback.blocks.tool.muted_collapsed);
        let style = if matches!(ctx.mode, DisplayMode::Collapsed) && muted_collapsed {
            theme.muted()
        } else {
            theme.primary()
        };
        let bold = style.add_modifier(ratatui::style::Modifier::BOLD);

        BlockOutput {
            lines: vec![Line::from(vec![Span::styled(self.name.clone(), bold)]).into()],
        }
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        false
    }

    fn accent(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        None
    }

    fn is_foldable(&self) -> bool {
        true
    }

    fn default_display_mode(&self) -> DisplayMode {
        DisplayMode::Collapsed
    }

    fn is_groupable(&self) -> bool {
        true
    }
}
