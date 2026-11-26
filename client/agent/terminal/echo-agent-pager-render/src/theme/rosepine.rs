use ratatui::style::{Color, Modifier};

use super::tokyonight::Theme;

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

#[allow(dead_code)]
mod palette {
    use super::*;

    pub const BASE: Color = rgb(35, 33, 54);
    pub const SURFACE: Color = rgb(42, 39, 63);
    pub const OVERLAY: Color = rgb(57, 53, 82);
    pub const MUTED: Color = rgb(110, 106, 134);
    pub const SUBTLE: Color = rgb(144, 140, 170);
    pub const TEXT: Color = rgb(224, 222, 244);
    pub const LOVE: Color = rgb(235, 111, 146);
    pub const GOLD: Color = rgb(246, 193, 119);
    pub const ROSE: Color = rgb(234, 154, 151);
    pub const PINE: Color = rgb(62, 143, 176);
    pub const FOAM: Color = rgb(156, 207, 216);
    pub const IRIS: Color = rgb(196, 167, 231);
    pub const HIGHLIGHT_LOW: Color = rgb(42, 40, 62);
    pub const HIGHLIGHT_MED: Color = rgb(68, 65, 90);
    pub const HIGHLIGHT_HIGH: Color = rgb(86, 82, 110);
}
use palette::*;

impl Theme {
    pub const fn rosepine_moon() -> Self {
        Self {
            bg_base: BASE,
            bg_light: OVERLAY,
            bg_dark: SURFACE,
            bg_highlight: OVERLAY,
            bg_hover: HIGHLIGHT_MED,
            bg_terminal: BASE,

            accent_user: TEXT,
            accent_assistant: IRIS,
            accent_thinking: MUTED,
            accent_tool: SUBTLE,
            accent_system: PINE,
            accent_error: LOVE,
            accent_success: FOAM,
            accent_running: MUTED,
            accent_skill: SUBTLE,

            text_primary: TEXT,
            text_secondary: SUBTLE,

            gray_dim: HIGHLIGHT_MED,
            gray: MUTED,
            gray_bright: SUBTLE,

            command: GOLD,
            path: ROSE,
            running: FOAM,
            warning: GOLD,

            fuzzy_accent: PINE,

            accent_plan: GOLD,

            accent_verify: PINE,

            accent_remember: PINE,

            selection_border: HIGHLIGHT_HIGH,
            hover_border: HIGHLIGHT_MED,
            prompt_border: HIGHLIGHT_MED,
            prompt_border_active: HIGHLIGHT_HIGH,

            accent_model: PINE,

            scrollbar_bg: HIGHLIGHT_LOW,
            scrollbar_fg: OVERLAY,

            diff_delete_bg: rgb(55, 30, 40),
            diff_delete_fg: LOVE,
            diff_insert_bg: rgb(25, 45, 55),
            diff_insert_fg: FOAM,
            diff_equal_fg: MUTED,
            diff_gutter_fg: MUTED,

            bg_visual: HIGHLIGHT_MED,

            paste_bg: SURFACE,
            paste_fg: SUBTLE,
            paste_dim: MUTED,

            md_heading_h1: TEXT,
            md_heading_h1_mod: Modifier::BOLD,
            md_heading_h2: FOAM,
            md_heading_h2_mod: Modifier::BOLD.union(Modifier::UNDERLINED),
            md_heading_h3: IRIS,
            md_heading_h3_mod: Modifier::BOLD,
            md_heading_h4: ROSE,
            md_heading_h4_mod: Modifier::BOLD.union(Modifier::ITALIC),
            md_heading_h5: GOLD,
            md_heading_h5_mod: Modifier::BOLD,
            md_heading_h6: PINE,
            md_heading_h6_mod: Modifier::BOLD,
            md_code: FOAM,
            md_task_checked: FOAM,
            md_task_unchecked: SUBTLE,
            md_muted: MUTED,
            md_code_bg: SURFACE,
            md_text: TEXT,
            link_fg: FOAM, // #9ccfd8 -- teal/cyan for dark bg
        }
    }
}
