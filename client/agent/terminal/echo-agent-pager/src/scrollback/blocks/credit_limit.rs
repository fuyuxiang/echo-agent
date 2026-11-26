//! CreditLimitBlock — scrollback card shown when a max-tier user exhausts credits.
//!
//! Replaces the Q&A question modal for users already at the highest tier
//! (EchoAgent Heavy). Instead of offering "Upgrade tier" + PAYG / buy-credits
//! options in the question overlay, this block renders an inline card with a
//! descriptive message and a link to the usage/billing page.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::appearance::AppearanceConfig;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{AccentStyle, BlockContext, BlockLine, BlockOutput, DisplayMode};
use crate::theme::Theme;

/// Which continue-path the max-tier credit-limit card recommends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreditLimitCardAction {
    /// Legacy on-demand: PAYG not enabled yet.
    EnablePayg,
    /// Legacy on-demand: PAYG on but at spending cap.
    IncreasePaygLimit,
    /// Unified usage billing: purchase prepaid credits.
    PurchaseCredits,
}

/// Inline scrollback card for credit-limit exhaustion on max-tier accounts.
#[derive(Debug, Clone)]
pub struct CreditLimitBlock {
    /// Card heading (e.g. "You've hit your free credits limit.").
    pub heading: String,
    /// Continue-path body copy selector.
    pub action: CreditLimitCardAction,
    /// URL to the usage/billing page.
    pub url: String,
}

impl CreditLimitBlock {
    /// Create a new credit-limit card.
    pub fn new(
        heading: impl Into<String>,
        action: CreditLimitCardAction,
        url: impl Into<String>,
    ) -> Self {
        Self {
            heading: heading.into(),
            action,
            url: url.into(),
        }
    }
}

impl BlockContent for CreditLimitBlock {
    fn output(&self, _ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();

        // Heading in bold warning color (amber/yellow).
        let heading_style = Style::default()
            .fg(theme.warning)
            .add_modifier(Modifier::BOLD);
        let heading = Line::from(Span::styled(self.heading.clone(), heading_style));

        // Body copy — contextual message based on billing mode.
        let muted = theme.muted();
        let body = match self.action {
            CreditLimitCardAction::IncreasePaygLimit => {
                "You can continue by increasing your spending limit."
            }
            CreditLimitCardAction::EnablePayg => {
                "You can continue by enabling pay-as-you-go usage."
            }
            CreditLimitCardAction::PurchaseCredits => {
                "You can continue by purchasing more credits."
            }
        };
        let body_line = Line::from(Span::styled(body.to_string(), muted));

        // Clickable link styled as a button.
        let link_style = theme.link_style();
        let link_line = Line::from(vec![Span::styled(self.url.clone(), link_style)]);

        BlockOutput {
            lines: vec![
                BlockLine::styled(heading).with_selection_range(Some(0)),
                BlockLine::separator(Line::from("")),
                BlockLine::styled(body_line).with_selection_range(Some(0)),
                BlockLine::styled(link_line).with_selection_range(Some(0)),
            ],
        }
    }

    fn accent(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        let theme = Theme::current();
        Some(AccentStyle::static_color(theme.warning))
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        true
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        false
    }

    fn default_display_mode(&self) -> DisplayMode {
        DisplayMode::Expanded
    }

    fn is_selectable(&self) -> bool {
        true
    }

    fn is_groupable(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::AppearanceConfig;

    fn ctx() -> BlockContext {
        BlockContext {
            mode: DisplayMode::Expanded,
            is_running: false,
            width: 80,
            raw: false,
            max_lines: None,
            appearance: AppearanceConfig::default(),
            is_selected: false,
            cwd: None,
        }
    }

    #[test]
    fn output_payg_off_mentions_enabling() {
        let block = CreditLimitBlock::new(
            "You\u{2019}ve hit your credit limit.",
            CreditLimitCardAction::EnablePayg,
            "https://cloud.echo-agent.invalid?_s=usage",
        );
        let output = block.output(&ctx());
        let all_text: String = output
            .lines
            .iter()
            .flat_map(|l| l.content.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("credit limit"));
        assert!(all_text.contains("enabling pay-as-you-go"));
        assert!(all_text.contains("cloud.echo-agent.invalid?_s=usage"));
    }

    #[test]
    fn output_payg_on_mentions_increasing() {
        let block = CreditLimitBlock::new(
            "You\u{2019}ve hit your spending cap.",
            CreditLimitCardAction::IncreasePaygLimit,
            "https://cloud.echo-agent.invalid?_s=usage",
        );
        let output = block.output(&ctx());
        let all_text: String = output
            .lines
            .iter()
            .flat_map(|l| l.content.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("spending cap"));
        assert!(all_text.contains("increasing your spending limit"));
        assert!(all_text.contains("cloud.echo-agent.invalid?_s=usage"));
    }

    #[test]
    fn output_unified_mentions_purchasing_credits() {
        let block = CreditLimitBlock::new(
            "You hit your weekly limit.",
            CreditLimitCardAction::PurchaseCredits,
            "https://cloud.echo-agent.invalid?_s=usage",
        );
        let output = block.output(&ctx());
        let all_text: String = output
            .lines
            .iter()
            .flat_map(|l| l.content.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains("purchasing more credits"));
        assert!(all_text.contains("cloud.echo-agent.invalid?_s=usage"));
    }

    #[test]
    fn has_warning_accent() {
        let block = CreditLimitBlock::new("heading", CreditLimitCardAction::EnablePayg, "url");
        let accent = block.accent(&ctx());
        let theme = Theme::current();
        assert!(accent.is_some());
        assert_eq!(accent.unwrap().color, theme.warning);
    }

    #[test]
    fn block_content_contract() {
        let block = CreditLimitBlock::new("heading", CreditLimitCardAction::EnablePayg, "url");
        let c = ctx();
        assert!(!block.is_foldable());
        assert!(block.is_selectable());
        assert!(!block.is_groupable());
        assert!(matches!(
            block.default_display_mode(),
            DisplayMode::Expanded
        ));
        assert!(block.has_vpad(&c));
        assert!(!block.has_raw_mode());
    }

    #[test]
    fn output_structure_and_content() {
        let url = "https://cloud.echo-agent.invalid?_s=usage";
        let block = CreditLimitBlock::new("Test heading", CreditLimitCardAction::EnablePayg, url);
        let output = block.output(&ctx());

        // heading, separator, body, link = 4 lines
        assert_eq!(output.lines.len(), 4);

        let all_text: String = output
            .lines
            .iter()
            .flat_map(|l| l.content.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all_text.contains(url));

        // Heading uses bold modifier.
        assert!(
            output.lines[0]
                .content
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn new_stores_fields_correctly() {
        let block = CreditLimitBlock::new(
            "my heading",
            CreditLimitCardAction::IncreasePaygLimit,
            "https://example.com",
        );
        assert_eq!(block.heading, "my heading");
        assert_eq!(block.action, CreditLimitCardAction::IncreasePaygLimit);
        assert_eq!(block.url, "https://example.com");
    }
}
