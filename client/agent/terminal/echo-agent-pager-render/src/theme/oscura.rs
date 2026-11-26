use ratatui::style::{Color, Modifier};

use super::tokyonight::Theme;

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Oscura Midnight palette.
///
/// Deep, dark backgrounds with a subtle purple/blue tint (OKLCH hue 265),
/// inspired by the Oscura Midnight palette (narative/oscura). Accent colors
/// lean purple to give the theme its distinctive identity.
///
/// Base colors were converted from OKLCH to sRGB programmatically via
/// the `coloraide` Python library. Purple accent colors are hand-picked
/// to complement the hue-265 background tint.
#[allow(dead_code)]
mod palette {
    use super::*;

    // -- backgrounds (OKLCH hue 265 backgrounds, OKLCH hue 265) -------
    pub const BASE: Color = rgb(3, 3, 4); // #030304  oklch(0.1 0.005 265)
    pub const SURFACE: Color = rgb(4, 5, 7); // #040507  oklch(0.115 0.005 265)
    pub const ELEVATED: Color = rgb(15, 18, 22); // #0F1216  oklch(0.18 0.01 265)
    pub const PANEL: Color = rgb(4, 4, 6); // #040406  oklch(0.11 0.006 265)

    // -- text (neutral, no color cast) ----------------------------------------
    pub const TEXT: Color = rgb(228, 228, 228); // #E4E4E4  oklch(0.92 0 0)
    pub const TEXT_DIM: Color = rgb(190, 190, 190); // #BEBEBE  oklch(0.8 0 0)

    // -- muted text (slight blue-purple tint) ---------------------------------
    pub const MUTED: Color = rgb(129, 134, 143); // #81868F  oklch(0.62 0.015 260)
    pub const SUBTLE: Color = rgb(94, 100, 108); // #5E646C  oklch(0.5 0.015 260)

    // -- semantic colors (from desktop action tokens) -------------------------
    pub const GOLD: Color = rgb(235, 217, 110); // #EBD96E  oklch(0.88 0.13 100)
    pub const RED: Color = rgb(220, 90, 100); // #DC5A64  muted rose-red
    pub const TEAL: Color = rgb(80, 180, 140); // #50B48C  softened teal
    pub const AMBER: Color = rgb(241, 189, 0); // #F1BD00  oklch(0.82 0.18 90)

    // -- purple accent ramp (the "purple hints") ------------------------------
    pub const PURPLE: Color = rgb(155, 126, 206); // #9B7ECE — signature purple
    pub const PURPLE_DIM: Color = rgb(110, 90, 154); // #6E5A9A — muted purple
    pub const PURPLE_BRIGHT: Color = rgb(196, 167, 231); // #C4A7E7 — vivid lavender

    // -- cyan (for running indicators, links) ---------------------------------
    pub const CYAN: Color = rgb(125, 207, 223); // #7DCFDF

    // -- highlight ramp (purple-tinted grays for UI chrome) -------------------
    pub const HIGHLIGHT_LOW: Color = rgb(18, 16, 28); // #12101C
    pub const HIGHLIGHT_MED: Color = rgb(36, 32, 52); // #242034
    pub const HIGHLIGHT_HIGH: Color = rgb(52, 48, 72); // #343048
}
use palette::*;

impl Theme {
    pub const fn oscura_midnight() -> Self {
        Self {
            bg_base: BASE,
            bg_light: ELEVATED,
            bg_dark: SURFACE,
            bg_highlight: ELEVATED,
            bg_hover: HIGHLIGHT_MED,
            bg_terminal: BASE,

            accent_user: PURPLE_BRIGHT,
            accent_assistant: PURPLE,
            accent_thinking: MUTED,
            accent_tool: SUBTLE,
            accent_system: CYAN,
            accent_error: RED,
            accent_success: TEAL,
            accent_running: PURPLE_DIM,
            accent_skill: PURPLE,

            text_primary: TEXT,
            text_secondary: TEXT_DIM,

            gray_dim: SUBTLE,
            gray: MUTED,
            gray_bright: TEXT_DIM,

            command: GOLD,
            path: AMBER,
            running: CYAN,
            warning: GOLD,

            fuzzy_accent: PURPLE_BRIGHT,

            accent_plan: GOLD,

            accent_verify: PURPLE,

            accent_remember: rgb(139, 195, 74), // #8BC34A — Material Design light green

            selection_border: HIGHLIGHT_HIGH,
            hover_border: HIGHLIGHT_MED,
            prompt_border: HIGHLIGHT_MED,
            prompt_border_active: HIGHLIGHT_HIGH,

            accent_model: CYAN,

            // Thumb must sit clearly above the track: `ELEVATED` (Σrgb 55)
            // was *darker* than the `HIGHLIGHT_LOW` track (Σrgb 62), which
            // made the scrollbar invisible — and follow mode blends the
            // thumb 40% toward the track, shrinking the delta further.
            // `HIGHLIGHT_HIGH` matches the weight of the theme's visible
            // chrome (selection border) and Rose Pine's thumb brightness.
            scrollbar_bg: HIGHLIGHT_LOW,
            scrollbar_fg: HIGHLIGHT_HIGH,

            diff_delete_bg: rgb(45, 15, 25),
            diff_delete_fg: RED,
            diff_insert_bg: rgb(10, 35, 30),
            diff_insert_fg: TEAL,
            diff_equal_fg: MUTED,
            diff_gutter_fg: MUTED,

            bg_visual: HIGHLIGHT_MED,

            paste_bg: SURFACE,
            paste_fg: TEXT_DIM,
            paste_dim: MUTED,

            md_heading_h1: TEXT,
            md_heading_h1_mod: Modifier::BOLD,
            md_heading_h2: PURPLE_BRIGHT,
            md_heading_h2_mod: Modifier::BOLD,
            md_heading_h3: PURPLE,
            md_heading_h3_mod: Modifier::BOLD,
            md_heading_h4: TEAL,
            md_heading_h4_mod: Modifier::BOLD.union(Modifier::ITALIC),
            md_heading_h5: GOLD,
            md_heading_h5_mod: Modifier::BOLD,
            md_heading_h6: CYAN,
            md_heading_h6_mod: Modifier::BOLD,
            md_code: CYAN,
            md_task_checked: TEAL,
            md_task_unchecked: TEXT_DIM,
            md_muted: MUTED,
            md_code_bg: SURFACE,
            md_text: TEXT,
            link_fg: CYAN,
        }
    }
}
