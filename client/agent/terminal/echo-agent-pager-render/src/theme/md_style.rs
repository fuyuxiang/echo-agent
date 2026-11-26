//! Theme-aware markdown rendering style.
//!
//! Defines the `MarkdownStyle` used by agent message and thinking blocks.
//! Colors come from the `md_*` fields on the current [`Theme`], which are
//! already quantized to the terminal's color capability level.

use anstyle::{Ansi256Color, AnsiColor, Color, Style};
use echo_agent_markdown::MarkdownStyle;

/// Convert `ratatui::style::Color` → `anstyle::Color` (type conversion only).
///
/// Quantization is already handled by [`Theme::current()`], so this just
/// bridges the two color types.
///
/// Returns `None` for `Reset`: `anstyle::Color` has no "terminal default"
/// variant, and downstream an unset color renders as the terminal default.
fn to_anstyle(c: ratatui::style::Color) -> Option<Color> {
    Some(match c {
        ratatui::style::Color::Reset => return None,
        ratatui::style::Color::Rgb(r, g, b) => Color::Rgb(anstyle::RgbColor(r, g, b)),
        ratatui::style::Color::Indexed(n) => Color::Ansi256(Ansi256Color(n)),
        // Named ANSI colors (from 16-color quantization).
        ratatui::style::Color::Black => Color::Ansi(AnsiColor::Black),
        ratatui::style::Color::Red => Color::Ansi(AnsiColor::Red),
        ratatui::style::Color::Green => Color::Ansi(AnsiColor::Green),
        ratatui::style::Color::Yellow => Color::Ansi(AnsiColor::Yellow),
        ratatui::style::Color::Blue => Color::Ansi(AnsiColor::Blue),
        ratatui::style::Color::Magenta => Color::Ansi(AnsiColor::Magenta),
        ratatui::style::Color::Cyan => Color::Ansi(AnsiColor::Cyan),
        ratatui::style::Color::Gray => Color::Ansi(AnsiColor::White),
        ratatui::style::Color::DarkGray => Color::Ansi(AnsiColor::BrightBlack),
        ratatui::style::Color::LightRed => Color::Ansi(AnsiColor::BrightRed),
        ratatui::style::Color::LightGreen => Color::Ansi(AnsiColor::BrightGreen),
        ratatui::style::Color::LightYellow => Color::Ansi(AnsiColor::BrightYellow),
        ratatui::style::Color::LightBlue => Color::Ansi(AnsiColor::BrightBlue),
        ratatui::style::Color::LightMagenta => Color::Ansi(AnsiColor::BrightMagenta),
        ratatui::style::Color::LightCyan => Color::Ansi(AnsiColor::BrightCyan),
        ratatui::style::Color::White => Color::Ansi(AnsiColor::BrightWhite),
    })
}

/// `anstyle::Style` with the given foreground color (converted from ratatui).
fn fg(c: ratatui::style::Color) -> Style {
    Style::new().fg_color(to_anstyle(c))
}

/// `anstyle::Style` with the given background color (converted from ratatui).
fn bg(c: ratatui::style::Color) -> Style {
    Style::new().bg_color(to_anstyle(c))
}

/// Convert `ratatui::style::Modifier` flags to `anstyle::Style` effects.
fn modifier_to_anstyle(m: ratatui::style::Modifier) -> Style {
    let mut s = Style::new();
    if m.contains(ratatui::style::Modifier::BOLD) {
        s = s.bold();
    }
    if m.contains(ratatui::style::Modifier::ITALIC) {
        s = s.italic();
    }
    if m.contains(ratatui::style::Modifier::UNDERLINED) {
        s = s.underline();
    }
    if m.contains(ratatui::style::Modifier::DIM) {
        s = s.dimmed();
    }
    if m.contains(ratatui::style::Modifier::HIDDEN) {
        s = s.hidden();
    }
    if m.contains(ratatui::style::Modifier::CROSSED_OUT) {
        s = s.strikethrough();
    }
    s
}

/// Build heading inner styles from theme colors and per-level modifiers.
fn heading_inner_styles(
    colors: [ratatui::style::Color; 6],
    mods: [ratatui::style::Modifier; 6],
) -> [Style; 6] {
    std::array::from_fn(|i| {
        let color_style = fg(colors[i]);
        let mod_style = modifier_to_anstyle(mods[i]);
        // Combine fg color with modifier effects.
        let mut s = color_style;
        let effects = mod_style.get_effects();
        if !effects.is_plain() {
            s = s.effects(s.get_effects() | effects);
        }
        s
    })
}

/// Build heading outer styles (dimmed + hidden, for syntax markers).
fn heading_outer_styles(colors: [ratatui::style::Color; 6]) -> [Style; 6] {
    colors.map(|c| fg(c).dimmed().hidden())
}

/// Get the theme-aware markdown style.
///
/// Built fresh from [`Theme::current()`] on each call. Both the theme
/// construction and style mapping are trivial struct copies.
pub fn style() -> MarkdownStyle {
    build_style()
}

fn build_style() -> MarkdownStyle {
    let theme = super::Theme::current();

    let heading_colors = [
        theme.md_heading_h1,
        theme.md_heading_h2,
        theme.md_heading_h3,
        theme.md_heading_h4,
        theme.md_heading_h5,
        theme.md_heading_h6,
    ];
    let heading_mods = [
        theme.md_heading_h1_mod,
        theme.md_heading_h2_mod,
        theme.md_heading_h3_mod,
        theme.md_heading_h4_mod,
        theme.md_heading_h5_mod,
        theme.md_heading_h6_mod,
    ];

    MarkdownStyle {
        heading_inner: heading_inner_styles(heading_colors, heading_mods),
        heading_outer: heading_outer_styles(heading_colors),
        strong_inner: fg(theme.md_text).bold(),
        strong_outer: Style::new().dimmed().hidden(),
        emphasis_inner: fg(theme.md_text).italic(),
        emphasis_outer: Style::new().dimmed().hidden(),
        strikethrough_inner: fg(theme.md_text).strikethrough(),
        strikethrough_outer: Style::new().dimmed().hidden(),
        inline_code_inner: fg(theme.md_code).bold(),
        inline_code_outer: fg(theme.md_code).dimmed().hidden(),
        // Selection-side bar detection (echo-agent-pager scrollback/blocks/
        // quote_bar.rs quote_bar_style) mirrors this exact style; its
        // end-to-end tests fail if this line changes.
        blockquote_outer: fg(theme.md_muted).dimmed(),
        task_checked: fg(theme.md_task_checked),
        task_unchecked: fg(theme.md_task_unchecked).dimmed(),
        list_item: fg(theme.md_muted),
        rule: fg(theme.md_muted),
        link_outer: fg(theme.md_muted),
        link_text: fg(theme.link_fg).underline(),
        link_url: fg(theme.md_muted),
        link_title: fg(theme.md_heading_h5),
        code_outer: fg(theme.md_code).dimmed().hidden(),
        code_language: fg(theme.md_heading_h3).hidden(),
        code_untagged: fg(theme.md_text),
        code_background: bg(theme.md_code_bg),
        table_outer: fg(theme.md_heading_h2).hidden(),
        text: fg(theme.md_text),
        math: fg(theme.md_text).italic(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `Reset` used to fall back to a concrete `AnsiColor::White`
    /// (ANSI-7 silver), which rendered Reset-themed markdown washed-out gray
    /// on light terminals and broke the `NO_COLOR` opt-out.
    #[test]
    fn reset_maps_to_no_color() {
        assert_eq!(to_anstyle(ratatui::style::Color::Reset), None);
        assert_eq!(fg(ratatui::style::Color::Reset).get_fg_color(), None);
        assert_eq!(bg(ratatui::style::Color::Reset).get_bg_color(), None);
    }

    /// Spot checks around the Gray/DarkGray naming mismatch between ratatui
    /// and anstyle.
    #[test]
    fn named_colors_map_concretely() {
        assert_eq!(
            to_anstyle(ratatui::style::Color::DarkGray),
            Some(Color::Ansi(AnsiColor::BrightBlack))
        );
        assert_eq!(
            to_anstyle(ratatui::style::Color::Gray),
            Some(Color::Ansi(AnsiColor::White))
        );
        assert_eq!(
            to_anstyle(ratatui::style::Color::Red),
            Some(Color::Ansi(AnsiColor::Red))
        );
    }

    #[test]
    fn terminal_default_body_text_has_no_fg() {
        let theme = super::super::Theme::terminal_default();
        assert_eq!(fg(theme.md_text).get_fg_color(), None);
        assert_eq!(bg(theme.md_code_bg).get_bg_color(), None);
    }
}
