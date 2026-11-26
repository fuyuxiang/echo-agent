#![allow(dead_code)]

use echo_agent_markdown::{MarkdownStyle, Syntect};

pub const fn rgb_color(r: u8, g: u8, b: u8) -> anstyle::Color {
    anstyle::Color::Rgb(anstyle::RgbColor(r, g, b))
}

pub const fn fg(color: anstyle::Color) -> anstyle::Style {
    anstyle::Style::new().fg_color(Some(color))
}

pub const fn bg(color: anstyle::Color) -> anstyle::Style {
    anstyle::Style::new().bg_color(Some(color))
}

pub const TEAL: anstyle::Color = rgb_color(26, 188, 156);
pub const BLUE: anstyle::Color = rgb_color(122, 162, 247);
pub const ORANGE: anstyle::Color = rgb_color(255, 158, 100);
pub const RED: anstyle::Color = rgb_color(247, 118, 142);
pub const GREEN: anstyle::Color = rgb_color(158, 206, 106);
pub const MAGENTA: anstyle::Color = rgb_color(187, 154, 247);
pub const YELLOW: anstyle::Color = rgb_color(224, 175, 104);
pub const CYAN: anstyle::Color = rgb_color(125, 207, 255);
pub const COMMENT: anstyle::Color = rgb_color(86, 95, 137);
pub const BG_DARK: anstyle::Color = rgb_color(31, 35, 53);

pub const HEADING_COLORS: [anstyle::Color; 6] = [TEAL, BLUE, ORANGE, RED, GREEN, MAGENTA];

pub const fn heading_styles(bold: bool, dimmed: bool, hidden: bool) -> [anstyle::Style; 6] {
    let mut styles = [anstyle::Style::new(); 6];
    let mut i = 0;
    while i < HEADING_COLORS.len() {
        styles[i] = fg(HEADING_COLORS[i]);
        if bold {
            styles[i] = styles[i].bold();
        }
        if dimmed {
            styles[i] = styles[i].dimmed();
        }
        if hidden {
            styles[i] = styles[i].hidden();
        }
        i += 1;
    }
    styles
}

pub const fn md_style(text: anstyle::Style) -> MarkdownStyle {
    MarkdownStyle {
        heading_inner: heading_styles(true, false, false),
        heading_outer: heading_styles(false, true, true),
        strong_inner: anstyle::Style::new().bold(),
        strong_outer: anstyle::Style::new().dimmed().hidden(),
        emphasis_inner: anstyle::Style::new().italic(),
        emphasis_outer: anstyle::Style::new().dimmed().hidden(),
        strikethrough_inner: anstyle::Style::new().strikethrough(),
        strikethrough_outer: anstyle::Style::new().dimmed().hidden(),
        inline_code_inner: fg(YELLOW).bold(),
        inline_code_outer: fg(YELLOW).dimmed().hidden(),
        blockquote_outer: fg(COMMENT).dimmed(),
        task_checked: fg(CYAN),
        task_unchecked: fg(BLUE).dimmed(),
        list_item: fg(BLUE).dimmed(),
        rule: fg(COMMENT),
        link_outer: fg(COMMENT),
        link_text: anstyle::Style::new().bold(),
        link_url: fg(COMMENT),
        link_title: fg(GREEN),
        code_outer: fg(YELLOW).dimmed().hidden(),
        code_language: fg(ORANGE).hidden(),
        code_untagged: anstyle::Style::new(),
        code_background: bg(BG_DARK),
        table_outer: fg(BLUE).hidden(),
        text,
        math: anstyle::Style::new().italic(),
    }
}

pub fn get_syntect() -> &'static Syntect {
    use std::sync::OnceLock;
    static SYNTECT: OnceLock<Syntect> = OnceLock::new();
    SYNTECT.get_or_init(|| Syntect::new(include_bytes!("../assets/tokyo-night.tmTheme")))
}
