//! Terminal-native palette for minimal mode.
//!
//! Any RGB theme is designed for one background polarity, so composited on
//! the terminal's own canvas it can land dark-on-dark or light-on-light
//! (e.g. macOS in Light Mode + a dark terminal profile). Polarity detection
//! is not reliable either: OS appearance and OSC 11 both disagree with the
//! actual canvas in edge cases and can change mid-session. Terminal
//! profiles, however, tune their **default** fg/bg to be legible against
//! their own background — this is how `git` and `ls` stay readable on any
//! terminal — so a palette built from `Reset` (body) + sparse named ANSI-16
//! accents is polarity-safe without detection.
//!
//! ## Grays / secondary text
//!
//! Do **not** paint body or status text as `DarkGray` (ANSI bright black).
//! Many dark profiles deliberately set that slot very dark for subtle
//! chrome, which washes out tool stdout and the prompt info bar. Instead:
//!
//! - **Primary content** (`text_primary`, `gray_bright`, …) → `Color::Reset`
//!   (terminal default foreground).
//! - **Secondary chrome** (`gray`, `gray_dim`, `text_secondary`) → also
//!   `Color::Reset`; [`Theme::muted`] / [`Theme::dim`] apply `Modifier::DIM`
//!   so de-emphasis tracks the terminal's own fg (polarity-safe), unlike
//!   hard-coding bright black.
//! - **Syntax highlighting** is not themed day/night. Under the native lock,
//!   syntect tokens are remapped via
//!   [`crate::syntax::polarity_safe_syntax_fg`] (default-fg grays + base ANSI
//!   accents). Do not load a light tmTheme based on OS/terminal detection.

use ratatui::style::{Color, Modifier};

use super::Theme;

impl Theme {
    /// The fixed terminal-native palette used by minimal mode: every field
    /// is `Color::Reset` or a named ANSI-16 color (see the module docs).
    pub const fn terminal_default() -> Self {
        // Secondary roles store Reset; Theme::muted / Theme::dim apply SGR dim.
        const MUTED: Color = Color::Reset;

        Self {
            bg_base: Color::Reset,
            bg_light: Color::Reset,
            bg_dark: Color::Reset,
            bg_highlight: Color::Reset,
            bg_hover: Color::Reset,
            bg_terminal: Color::Reset,

            accent_user: Color::Reset,
            accent_assistant: Color::Magenta,
            accent_thinking: MUTED,
            accent_tool: MUTED,
            accent_system: Color::Blue,
            accent_error: Color::Red,
            accent_success: Color::Green,
            accent_running: Color::Magenta,
            accent_skill: Color::Blue,

            text_primary: Color::Reset,
            text_secondary: MUTED,

            gray_dim: MUTED,
            gray: MUTED,
            gray_bright: Color::Reset,

            command: Color::Yellow,
            path: Color::Cyan,
            running: Color::Cyan,
            warning: Color::Yellow,

            fuzzy_accent: Color::Cyan,

            accent_plan: Color::Yellow,
            accent_verify: Color::Magenta,
            accent_remember: Color::Green,

            selection_border: MUTED,
            hover_border: MUTED,
            prompt_border: MUTED,
            prompt_border_active: Color::Reset,

            accent_model: Color::Cyan,

            scrollbar_bg: Color::Reset,
            scrollbar_fg: MUTED,

            diff_delete_bg: Color::Reset,
            diff_delete_fg: Color::Red,
            diff_insert_bg: Color::Reset,
            diff_insert_fg: Color::Green,
            diff_equal_fg: MUTED,
            diff_gutter_fg: MUTED,

            bg_visual: Color::Reset,

            paste_bg: Color::Reset,
            paste_fg: MUTED,
            paste_dim: MUTED,

            md_heading_h1: Color::Reset,
            md_heading_h1_mod: Modifier::BOLD.union(Modifier::UNDERLINED),
            md_heading_h2: Color::Reset,
            md_heading_h2_mod: Modifier::BOLD,
            md_heading_h3: Color::Reset,
            md_heading_h3_mod: Modifier::BOLD,
            md_heading_h4: Color::Reset,
            md_heading_h4_mod: Modifier::BOLD,
            md_heading_h5: Color::Reset,
            md_heading_h5_mod: Modifier::BOLD,
            md_heading_h6: MUTED,
            md_heading_h6_mod: Modifier::BOLD,
            md_code: Color::Cyan,
            md_task_checked: Color::Green,
            md_task_unchecked: MUTED,
            md_muted: MUTED,
            md_code_bg: Color::Reset,
            md_text: Color::Reset,
            link_fg: Color::Blue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_colors(theme: &Theme) -> Vec<(&'static str, Color)> {
        vec![
            ("bg_base", theme.bg_base),
            ("bg_light", theme.bg_light),
            ("bg_dark", theme.bg_dark),
            ("bg_highlight", theme.bg_highlight),
            ("bg_hover", theme.bg_hover),
            ("bg_terminal", theme.bg_terminal),
            ("accent_user", theme.accent_user),
            ("accent_assistant", theme.accent_assistant),
            ("accent_thinking", theme.accent_thinking),
            ("accent_tool", theme.accent_tool),
            ("accent_system", theme.accent_system),
            ("accent_error", theme.accent_error),
            ("accent_success", theme.accent_success),
            ("accent_running", theme.accent_running),
            ("accent_skill", theme.accent_skill),
            ("text_primary", theme.text_primary),
            ("text_secondary", theme.text_secondary),
            ("gray_dim", theme.gray_dim),
            ("gray", theme.gray),
            ("gray_bright", theme.gray_bright),
            ("command", theme.command),
            ("path", theme.path),
            ("running", theme.running),
            ("warning", theme.warning),
            ("fuzzy_accent", theme.fuzzy_accent),
            ("accent_plan", theme.accent_plan),
            ("accent_verify", theme.accent_verify),
            ("accent_remember", theme.accent_remember),
            ("selection_border", theme.selection_border),
            ("hover_border", theme.hover_border),
            ("prompt_border", theme.prompt_border),
            ("prompt_border_active", theme.prompt_border_active),
            ("accent_model", theme.accent_model),
            ("scrollbar_bg", theme.scrollbar_bg),
            ("scrollbar_fg", theme.scrollbar_fg),
            ("diff_delete_bg", theme.diff_delete_bg),
            ("diff_delete_fg", theme.diff_delete_fg),
            ("diff_insert_bg", theme.diff_insert_bg),
            ("diff_insert_fg", theme.diff_insert_fg),
            ("diff_equal_fg", theme.diff_equal_fg),
            ("diff_gutter_fg", theme.diff_gutter_fg),
            ("bg_visual", theme.bg_visual),
            ("paste_bg", theme.paste_bg),
            ("paste_fg", theme.paste_fg),
            ("paste_dim", theme.paste_dim),
            ("md_heading_h1", theme.md_heading_h1),
            ("md_heading_h2", theme.md_heading_h2),
            ("md_heading_h3", theme.md_heading_h3),
            ("md_heading_h4", theme.md_heading_h4),
            ("md_heading_h5", theme.md_heading_h5),
            ("md_heading_h6", theme.md_heading_h6),
            ("md_code", theme.md_code),
            ("md_task_checked", theme.md_task_checked),
            ("md_task_unchecked", theme.md_task_unchecked),
            ("md_muted", theme.md_muted),
            ("md_code_bg", theme.md_code_bg),
            ("md_text", theme.md_text),
            ("link_fg", theme.link_fg),
        ]
    }

    #[test]
    fn terminal_default_uses_only_reset_and_named_ansi() {
        let theme = Theme::terminal_default();
        for (name, color) in all_colors(&theme) {
            assert!(
                !matches!(color, Color::Rgb(..) | Color::Indexed(_)),
                "{name} must be Reset or a named ANSI color, got {color:?}"
            );
        }
    }

    #[test]
    fn terminal_default_backgrounds_are_transparent() {
        let theme = Theme::terminal_default();
        for (name, color) in [
            ("bg_base", theme.bg_base),
            ("bg_light", theme.bg_light),
            ("bg_dark", theme.bg_dark),
            ("bg_terminal", theme.bg_terminal),
            ("md_code_bg", theme.md_code_bg),
            ("diff_delete_bg", theme.diff_delete_bg),
            ("diff_insert_bg", theme.diff_insert_bg),
            ("paste_bg", theme.paste_bg),
        ] {
            assert_eq!(color, Color::Reset, "{name} must defer to the canvas");
        }
    }

    #[test]
    fn terminal_default_leaves_cursor_color_alone() {
        let theme = Theme::terminal_default();
        assert_eq!(theme.accent_user, Color::Reset);
        assert_eq!(
            crate::render::color::resolve_to_rgb(theme.accent_user),
            None
        );
    }

    #[test]
    fn terminal_default_survives_quantization() {
        use crate::theme::color_support::ColorLevel;
        let theme = Theme::terminal_default();
        for level in [
            ColorLevel::Basic,
            ColorLevel::Ansi256,
            ColorLevel::TrueColor,
        ] {
            let quantized = theme.quantized(level);
            for ((name, before), (_, after)) in
                all_colors(&theme).into_iter().zip(all_colors(&quantized))
            {
                assert_eq!(before, after, "{name} must survive {level:?}");
            }
        }
        let stripped = theme.quantized(ColorLevel::None);
        for (name, color) in all_colors(&stripped) {
            assert_eq!(color, Color::Reset, "{name} must strip under NO_COLOR");
        }
    }

    #[test]
    fn terminal_default_primary_is_reset_not_dark_gray() {
        let theme = Theme::terminal_default();
        assert_eq!(theme.text_primary, Color::Reset);
        assert_eq!(theme.gray, Color::Reset);
        assert_eq!(theme.gray_dim, Color::Reset);
        // Must not hard-code bright black for body/secondary roles.
        assert_ne!(theme.text_primary, Color::DarkGray);
        assert_ne!(theme.gray, Color::DarkGray);
        assert_ne!(theme.gray_dim, Color::DarkGray);
    }

    #[test]
    fn terminal_default_muted_and_dim_use_sgr_dim_not_bright_black() {
        use ratatui::style::Modifier;
        let theme = Theme::terminal_default();
        let muted = theme.muted();
        let dim = theme.dim();
        assert!(
            muted.add_modifier.contains(Modifier::DIM),
            "muted should DIM the terminal default fg: {muted:?}"
        );
        assert!(
            dim.add_modifier.contains(Modifier::DIM),
            "dim should DIM the terminal default fg: {dim:?}"
        );
        // No explicit DarkGray paint — dim tracks the host palette.
        assert!(
            muted.fg.is_none() || muted.fg == Some(Color::Reset),
            "muted must not set a hard gray: {muted:?}"
        );
        assert!(
            dim.fg.is_none() || dim.fg == Some(Color::Reset),
            "dim must not set a hard gray: {dim:?}"
        );
    }

    #[test]
    fn rgb_theme_muted_keeps_explicit_gray_without_forced_dim() {
        use ratatui::style::Modifier;
        // EchoAgentNight paints real RGB grays; muted/dim must not invent DIM.
        let theme = Theme::echo_agent_night();
        assert!(!matches!(theme.gray, Color::Reset));
        let muted = theme.muted();
        assert_eq!(muted.fg, Some(theme.gray));
        assert!(
            !muted.add_modifier.contains(Modifier::DIM),
            "RGB muted should not force SGR dim: {muted:?}"
        );
    }
}
