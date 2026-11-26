//! Markdown styling types.
//!
//! This module provides the `MarkdownStyle` struct which defines colors and
//! effects for all markdown elements.

use anstyle::{Effects, Style};

use crate::colors::adapt_style;

/// Table border characters for rendering tables in pretty mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TableBorders {
    chars: [char; 11],
}

impl TableBorders {
    const H: usize = 0;
    const V: usize = 1;
    const TL: usize = 2;
    const TR: usize = 3;
    const BL: usize = 4;
    const BR: usize = 5;
    const T_T: usize = 6;
    const T_B: usize = 7;
    const T_L: usize = 8;
    const T_R: usize = 9;
    const X: usize = 10;

    pub const BOX: Self = Self {
        chars: ['─', '│', '┌', '┐', '└', '┘', '┬', '┴', '├', '┤', '┼'],
    };

    pub const ASCII: Self = Self {
        chars: ['-', '|', '+', '+', '+', '+', '+', '+', '+', '+', '+'],
    };

    pub const DOUBLE: Self = Self {
        chars: ['═', '║', '╔', '╗', '╚', '╝', '╦', '╩', '╠', '╣', '╬'],
    };

    pub const fn new(chars: [char; 11]) -> Self {
        Self { chars }
    }

    // Short names (used in table formatting)
    pub const fn h(&self) -> char {
        self.chars[Self::H]
    }
    pub const fn v(&self) -> char {
        self.chars[Self::V]
    }
    pub const fn c_tl(&self) -> char {
        self.chars[Self::TL]
    }
    pub const fn c_tr(&self) -> char {
        self.chars[Self::TR]
    }
    pub const fn c_bl(&self) -> char {
        self.chars[Self::BL]
    }
    pub const fn c_br(&self) -> char {
        self.chars[Self::BR]
    }
    pub const fn t_t(&self) -> char {
        self.chars[Self::T_T]
    }
    pub const fn t_b(&self) -> char {
        self.chars[Self::T_B]
    }
    pub const fn t_l(&self) -> char {
        self.chars[Self::T_L]
    }
    pub const fn t_r(&self) -> char {
        self.chars[Self::T_R]
    }
    pub const fn x(&self) -> char {
        self.chars[Self::X]
    }

    // Long names (for readability)
    pub const fn horizontal(&self) -> char {
        self.chars[Self::H]
    }
    pub const fn vertical(&self) -> char {
        self.chars[Self::V]
    }
    pub const fn top_left(&self) -> char {
        self.chars[Self::TL]
    }
    pub const fn top_right(&self) -> char {
        self.chars[Self::TR]
    }
    pub const fn bottom_left(&self) -> char {
        self.chars[Self::BL]
    }
    pub const fn bottom_right(&self) -> char {
        self.chars[Self::BR]
    }
    pub const fn t_top(&self) -> char {
        self.chars[Self::T_T]
    }
    pub const fn t_bottom(&self) -> char {
        self.chars[Self::T_B]
    }
    pub const fn t_left(&self) -> char {
        self.chars[Self::T_L]
    }
    pub const fn t_right(&self) -> char {
        self.chars[Self::T_R]
    }
    pub const fn cross(&self) -> char {
        self.chars[Self::X]
    }
}

impl Default for TableBorders {
    fn default() -> Self {
        Self::BOX
    }
}

/// Style configuration for markdown rendering.
///
/// Each field controls the styling for a specific markdown element.
/// The `_inner` variants are applied to the content, while `_outer` variants
/// are applied to the syntax markers (which are hidden in pretty mode).
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq, Hash)]
pub struct MarkdownStyle {
    pub heading_inner: [Style; 6],
    pub heading_outer: [Style; 6],
    pub strong_inner: Style,
    pub strong_outer: Style,
    pub emphasis_inner: Style,
    pub emphasis_outer: Style,
    pub strikethrough_inner: Style,
    pub strikethrough_outer: Style,
    pub inline_code_inner: Style,
    pub inline_code_outer: Style,
    pub blockquote_outer: Style,
    pub task_checked: Style,
    pub task_unchecked: Style,
    pub list_item: Style,
    pub rule: Style,
    pub link_outer: Style,
    pub link_text: Style,
    pub link_url: Style,
    pub link_title: Style,
    pub code_outer: Style,
    pub code_language: Style,
    pub code_untagged: Style,
    pub code_background: Style,
    pub table_outer: Style,
    /// Default foreground for plain body text (paragraphs with no formatting).
    /// When set, the renderer applies this to text spans that would otherwise
    /// inherit the terminal's default foreground.
    pub text: Style,
    /// Style for rendered LaTeX math (inline `$...$`/`\(...\)` content after
    /// Unicode conversion, and display math block lines). In raw mode the
    /// style applies to the unconverted TeX source.
    pub math: Style,
}

impl MarkdownStyle {
    /// Adapt all styles for the terminal's color capabilities.
    ///
    /// This downgrades RGB colors to 256-color or 16-color as needed.
    pub fn adapt(self) -> Self {
        Self {
            heading_inner: [
                adapt_style(self.heading_inner[0]),
                adapt_style(self.heading_inner[1]),
                adapt_style(self.heading_inner[2]),
                adapt_style(self.heading_inner[3]),
                adapt_style(self.heading_inner[4]),
                adapt_style(self.heading_inner[5]),
            ],
            heading_outer: [
                adapt_style(self.heading_outer[0]),
                adapt_style(self.heading_outer[1]),
                adapt_style(self.heading_outer[2]),
                adapt_style(self.heading_outer[3]),
                adapt_style(self.heading_outer[4]),
                adapt_style(self.heading_outer[5]),
            ],
            strong_inner: adapt_style(self.strong_inner),
            strong_outer: adapt_style(self.strong_outer),
            emphasis_inner: adapt_style(self.emphasis_inner),
            emphasis_outer: adapt_style(self.emphasis_outer),
            strikethrough_inner: adapt_style(self.strikethrough_inner),
            strikethrough_outer: adapt_style(self.strikethrough_outer),
            inline_code_inner: adapt_style(self.inline_code_inner),
            inline_code_outer: adapt_style(self.inline_code_outer),
            blockquote_outer: adapt_style(self.blockquote_outer),
            task_checked: adapt_style(self.task_checked),
            task_unchecked: adapt_style(self.task_unchecked),
            list_item: adapt_style(self.list_item),
            rule: adapt_style(self.rule),
            link_outer: adapt_style(self.link_outer),
            link_text: adapt_style(self.link_text),
            link_url: adapt_style(self.link_url),
            link_title: adapt_style(self.link_title),
            code_outer: adapt_style(self.code_outer),
            code_language: adapt_style(self.code_language),
            code_untagged: adapt_style(self.code_untagged),
            code_background: adapt_style(self.code_background),
            table_outer: adapt_style(self.table_outer),
            text: adapt_style(self.text),
            math: adapt_style(self.math),
        }
    }
}

/// Check if ALL active styles have HIDDEN effect.
/// Used in pretty mode to determine if text should be skipped.
pub(crate) fn all_hidden(styles: impl IntoIterator<Item = Option<Style>>) -> bool {
    let mut has_any = false;
    let mut all_are_hidden = true;

    for style in styles {
        has_any = true;
        match style {
            Some(s) if s.get_effects().contains(Effects::HIDDEN) => {}
            _ => {
                all_are_hidden = false;
            }
        }
    }

    has_any && all_are_hidden
}

/// Merge multiple styles into one for rendering.
/// Strips HIDDEN from final output - it's a semantic marker, not a visual style.
pub(crate) fn merge_styles(styles: impl IntoIterator<Item = Option<Style>>) -> Style {
    let mut out = Style::new();
    let mut prev = Style::new();
    for style in styles {
        if out.get_effects().contains(Effects::HIDDEN) {
            out = prev;
        } else {
            prev = out;
        }
        let Some(style) = style else {
            continue;
        };
        if !style.get_effects().is_plain() {
            out = out.effects(out.get_effects() | style.get_effects());
        }
        if style.get_effects().contains(Effects::DIMMED) {
            out = out.effects(out.get_effects().remove(Effects::BOLD));
        }
        if style.get_effects().contains(Effects::BOLD) {
            out = out.effects(out.get_effects().remove(Effects::DIMMED));
        }
        if let Some(color) = style.get_fg_color() {
            out = out.fg_color(Some(color));
        }
        if let Some(color) = style.get_bg_color() {
            out = out.bg_color(Some(color));
        }
        if let Some(color) = style.get_underline_color() {
            out = out.underline_color(Some(color));
        }
    }
    out.effects(out.get_effects().remove(Effects::HIDDEN))
}

// Simple default style for testing (no colors, just effects)
#[cfg(any(test, fuzzing))]
pub mod test_style {
    use super::MarkdownStyle;
    use anstyle::Style;

    /// A minimal style for testing with no colors.
    pub const STYLE: MarkdownStyle = MarkdownStyle {
        heading_inner: [Style::new().bold(); 6],
        heading_outer: [Style::new().dimmed().hidden(); 6],
        strong_inner: Style::new().bold(),
        strong_outer: Style::new().dimmed().hidden(),
        emphasis_inner: Style::new().italic(),
        emphasis_outer: Style::new().dimmed().hidden(),
        strikethrough_inner: Style::new().strikethrough(),
        strikethrough_outer: Style::new().dimmed().hidden(),
        inline_code_inner: Style::new().bold(),
        inline_code_outer: Style::new().dimmed().hidden(),
        blockquote_outer: Style::new().dimmed(),
        task_checked: Style::new(),
        task_unchecked: Style::new().dimmed(),
        list_item: Style::new().dimmed(),
        rule: Style::new(),
        link_outer: Style::new(),
        link_text: Style::new().bold(),
        link_url: Style::new().dimmed(),
        link_title: Style::new(),
        code_outer: Style::new().dimmed().hidden(),
        code_language: Style::new().hidden(),
        code_untagged: Style::new(),
        code_background: Style::new(),
        table_outer: Style::new().bold(),
        text: Style::new(),
        math: Style::new().italic(),
    };
}
