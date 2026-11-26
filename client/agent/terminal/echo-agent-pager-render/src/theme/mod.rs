//! Theming for the pager.
//!
//! All colors come from the `Theme` struct. No hardcoded colors elsewhere.
//! The default theme is EchoAgentNight (neutral gray base with TokyoNight accents).
//!
//! ## Color support
//!
//! EchoAgentNight is defined in `Color::Rgb` (truecolor). At startup,
//! [`Theme::current()`] quantizes every color to the terminal's detected
//! capability level via [`Theme::quantized`]. Runtime-generated colors (syntax
//! highlighting, blending) are also quantized via [`color_support::quantize`].

pub mod cache;
pub mod color_support;
mod echo_agent_day;
mod echo_agent_night;
pub mod env_appearance;
pub mod md_style;
pub mod osc11;
mod oscura;
mod rosepine;
pub mod system_appearance;
mod terminal_default;
pub mod tokyonight;

pub use color_support::quantize;
pub use tokyonight::{Theme, pulse_brightness, wave_brightness};

const LEGACY_NIGHT_THEME_NAME: &str = concat!("g", "roknight");
const LEGACY_DAY_THEME_NAME: &str = concat!("g", "rokday");

/// Available theme variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeKind {
    EchoAgentNight = 0,
    EchoAgentDay = 1,
    TokyoNight = 2,
    RosePineMoon = 3,
    OscuraMidnight = 5,
    /// Meta-variant: follow system dark/light appearance.
    ///
    /// Never stored in `cache::CURRENT` — resolved to a concrete
    /// theme at startup and on live appearance changes. The `"auto"`
    /// string is stored on disk and in `app.current_ui.theme`, but
    /// only the resolved concrete kind lives in the cache.
    /// Excluded from [`ALL`] and [`available()`].
    Auto = 4,
}

impl ThemeKind {
    /// All theme kinds (including those that may not work on the current terminal).
    pub const ALL: &[ThemeKind] = &[
        ThemeKind::EchoAgentNight,
        ThemeKind::EchoAgentDay,
        ThemeKind::TokyoNight,
        ThemeKind::RosePineMoon,
        ThemeKind::OscuraMidnight,
    ];

    /// Theme kinds available on the current terminal.
    ///
    /// Filters out themes that require truecolor when the terminal
    /// does not support it (e.g., macOS Terminal.app is 256-color).
    pub fn available() -> &'static [ThemeKind] {
        // Two possible results — pick the right const slice based on
        // the detected color level. No heap allocation needed.
        const ALL: &[ThemeKind] = ThemeKind::ALL;
        const NO_TRUECOLOR: &[ThemeKind] = &[ThemeKind::EchoAgentNight, ThemeKind::EchoAgentDay];

        if color_support::detect().has_truecolor() {
            ALL
        } else {
            NO_TRUECOLOR
        }
    }

    /// Human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::EchoAgentNight => "echo-agent-night",
            Self::TokyoNight => "tokyonight",
            Self::EchoAgentDay => "echo-agent-day",
            Self::RosePineMoon => "rosepine-moon",
            Self::OscuraMidnight => "oscura-midnight",
            Self::Auto => "auto",
        }
    }

    /// Whether this theme requires truecolor (24-bit RGB) to look correct.
    ///
    /// TokyoNight uses blue-tinted backgrounds that lose their character
    /// when quantized to 256 or 16 colors. EchoAgentNight uses neutral grays
    /// that survive quantization cleanly.
    pub fn requires_truecolor(self) -> bool {
        match self {
            Self::EchoAgentNight => false,
            Self::TokyoNight => true,
            Self::EchoAgentDay => false,
            Self::RosePineMoon => true,
            Self::OscuraMidnight => true,
            // Auto is resolved to a concrete theme before rendering.
            Self::Auto => false,
        }
    }

    /// Parse a theme name (case-insensitive). All string→ThemeKind
    /// conversions must go through this function.
    pub fn from_name(name: &str) -> Option<Self> {
        let lower = name.to_lowercase();
        if lower == LEGACY_NIGHT_THEME_NAME {
            return Some(Self::EchoAgentNight);
        }
        if lower == LEGACY_DAY_THEME_NAME {
            return Some(Self::EchoAgentDay);
        }
        match lower.as_str() {
            "auto" | "system" => Some(Self::Auto),
            "echo-agent-night" | "dark" => Some(Self::EchoAgentNight),
            "tokyonight" | "tokyo-night" | "tokyo" => Some(Self::TokyoNight),
            "echo-agent-day" | "light" | "day" => Some(Self::EchoAgentDay),
            "rosepine" | "rose-pine" | "rosepine-moon" | "rose-pine-moon" => {
                Some(Self::RosePineMoon)
            }
            "oscura" | "oscura-midnight" => Some(Self::OscuraMidnight),
            _ => None,
        }
    }

    /// Whether this is the meta "auto" variant (resolved at runtime).
    #[must_use]
    pub fn is_auto(self) -> bool {
        self == Self::Auto
    }
}

/// `FromStr` wrapper around [`ThemeKind::from_name`].
impl std::str::FromStr for ThemeKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_name(s).ok_or(())
    }
}

/// Resolve a theme string to its canonical `&'static str` name.
/// Used by both dispatch and registry layers.
pub fn canonical_name(value: &str) -> Option<&'static str> {
    ThemeKind::from_name(value).map(|k| k.display_name())
}

/// Human-friendly display name for a canonical theme value (e.g.
/// `"echo-agent-night"` → `"EchoAgent Night"`). Falls back to `value` verbatim.
pub fn display_name_for_canonical(value: &str) -> &str {
    match value {
        "auto" => "Auto",
        "echo-agent-night" => "EchoAgent Night",
        "echo-agent-day" => "EchoAgent Day",
        "tokyonight" => "Tokyo Night",
        "rosepine-moon" => "Rose Pine Moon",
        other => other,
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::echo_agent_night()
    }
}

impl Theme {
    /// Return a copy with every color quantized to the given level.
    ///
    /// This adapts the theme to terminals with limited color support.
    /// On truecolor terminals the RGB values pass through unchanged;
    /// on 256-color terminals they are mapped to the nearest indexed
    /// palette entry; on 16-color terminals they map to ANSI names.
    pub fn quantized(self, level: color_support::ColorLevel) -> Self {
        use color_support::quantize_color;
        let q = |c: ratatui::style::Color| quantize_color(c, level);
        Self {
            bg_base: q(self.bg_base),
            bg_light: q(self.bg_light),
            bg_dark: q(self.bg_dark),
            bg_highlight: q(self.bg_highlight),
            bg_hover: q(self.bg_hover),
            bg_terminal: q(self.bg_terminal),

            accent_user: q(self.accent_user),
            accent_assistant: q(self.accent_assistant),
            accent_thinking: q(self.accent_thinking),
            accent_tool: q(self.accent_tool),
            accent_system: q(self.accent_system),
            accent_error: q(self.accent_error),
            accent_success: q(self.accent_success),
            accent_running: q(self.accent_running),
            accent_skill: q(self.accent_skill),

            text_primary: q(self.text_primary),
            text_secondary: q(self.text_secondary),

            gray_dim: q(self.gray_dim),
            gray: q(self.gray),
            gray_bright: q(self.gray_bright),

            command: q(self.command),
            path: q(self.path),
            running: q(self.running),
            warning: q(self.warning),

            fuzzy_accent: q(self.fuzzy_accent),

            accent_plan: q(self.accent_plan),

            accent_verify: q(self.accent_verify),

            accent_remember: q(self.accent_remember),

            selection_border: q(self.selection_border),
            hover_border: q(self.hover_border),
            prompt_border: q(self.prompt_border),
            prompt_border_active: q(self.prompt_border_active),

            accent_model: q(self.accent_model),

            scrollbar_bg: q(self.scrollbar_bg),
            scrollbar_fg: q(self.scrollbar_fg),

            diff_delete_bg: q(self.diff_delete_bg),
            diff_delete_fg: q(self.diff_delete_fg),
            diff_insert_bg: q(self.diff_insert_bg),
            diff_insert_fg: q(self.diff_insert_fg),
            diff_equal_fg: q(self.diff_equal_fg),
            diff_gutter_fg: q(self.diff_gutter_fg),

            bg_visual: q(self.bg_visual),

            paste_bg: q(self.paste_bg),
            paste_fg: q(self.paste_fg),
            paste_dim: q(self.paste_dim),

            md_heading_h1: q(self.md_heading_h1),
            md_heading_h1_mod: self.md_heading_h1_mod,
            md_heading_h2: q(self.md_heading_h2),
            md_heading_h2_mod: self.md_heading_h2_mod,
            md_heading_h3: q(self.md_heading_h3),
            md_heading_h3_mod: self.md_heading_h3_mod,
            md_heading_h4: q(self.md_heading_h4),
            md_heading_h4_mod: self.md_heading_h4_mod,
            md_heading_h5: q(self.md_heading_h5),
            md_heading_h5_mod: self.md_heading_h5_mod,
            md_heading_h6: q(self.md_heading_h6),
            md_heading_h6_mod: self.md_heading_h6_mod,
            md_code: q(self.md_code),
            md_task_checked: q(self.md_task_checked),
            md_task_unchecked: q(self.md_task_unchecked),
            md_muted: q(self.md_muted),
            md_code_bg: q(self.md_code_bg),
            md_text: q(self.md_text),
            link_fg: q(self.link_fg),
        }
    }

    /// Get the current theme, quantized to the terminal's color level.
    ///
    /// Reads the active theme kind (loaded from `~/.echo-agent/config.toml` on
    /// first call, then cached in memory), builds the theme from its
    /// `const fn` constructor, and quantizes to the terminal's color level.
    ///
    /// On Windows applies a contrast boost so structural RGB survives the
    /// display gamma. At [`ColorLevel::Basic`] (or legacy ConHost below
    /// truecolor) we additionally pin chrome colors to ANSI-named entries
    /// because every dark RGB collapses onto the same ANSI16 slot otherwise.
    /// Modern ConHost (Win10 1709+) lands on TrueColor via
    /// [`color_support::terminal_supports_truecolor`] and skips the overrides.
    pub fn current() -> Self {
        let level = color_support::detect();
        if cache::terminal_native_locked() {
            return Self::terminal_default().quantized(level);
        }
        let base = match cache::current_kind() {
            ThemeKind::EchoAgentNight => Self::echo_agent_night(),
            ThemeKind::TokyoNight => Self::tokyonight(),
            ThemeKind::EchoAgentDay => Self::echo_agent_day(),
            ThemeKind::RosePineMoon => Self::rosepine_moon(),
            ThemeKind::OscuraMidnight => Self::oscura_midnight(),
            // Auto is resolved to a concrete theme before being stored;
            // if reached, fall back to EchoAgentNight.
            ThemeKind::Auto => Self::echo_agent_night(),
        };
        // Sample polarity pre-quantization — post-quantize `bg_base` may
        // land on a named/indexed entry whose luminance is host-palette-
        // dependent.
        let dark = base.is_dark();
        let adapted = if cfg!(target_os = "windows") {
            base.windows_contrast_boost(dark)
        } else {
            base
        };
        let adapted = adapted.quantized(level);
        // ANSI16 chrome fallback — fires in two cases:
        //   1. Any terminal that only advertises 16-color support
        //      (e.g., `TERM=xterm`, `TERM=ansi`, or `ECHO_AGENT_FORCE_COLOR_LEVEL=basic`),
        //      where naive quantization collapses every dark RGB onto `Color::Black`.
        //   2. Legacy Windows ConHost below TrueColor, kept for parity with the
        //      glyph fallback path also gated on `is_legacy_windows_console()`.
        //
        // Both arms require `has_color()` so that `NO_COLOR` (which produces
        // `ColorLevel::None`) keeps suppressing all SGR output. Without the
        // explicit gate on the legacy-Windows arm, `ansi16_chrome_overrides`
        // would repaint `Color::Reset` slots with named ANSI colors and
        // partially defeat the user's opt-out on ConHost.
        if level.has_color()
            && (level == color_support::ColorLevel::Basic
                || (crate::glyphs::is_legacy_windows_console() && !level.has_truecolor()))
        {
            adapted.ansi16_chrome_overrides(dark)
        } else {
            adapted
        }
    }

    /// Get the currently active theme kind.
    pub fn current_kind() -> ThemeKind {
        cache::current_kind()
    }

    /// Whether this theme paints no diff row bands (`diff_*_bg` = `Reset`),
    /// in which case changed diff lines carry a whole-line red/green
    /// *foreground* instead of syntax highlighting on a colored band.
    #[must_use]
    pub fn diff_uses_line_fg(&self) -> bool {
        use ratatui::style::Color;
        self.diff_delete_bg == Color::Reset && self.diff_insert_bg == Color::Reset
    }

    /// Apply a theme kind to the in-memory state without persisting.
    /// Used by the dispatcher, live-preview, and the appearance watcher.
    ///
    /// No-op while the terminal-native lock is engaged.
    pub fn apply_kind(kind: ThemeKind) -> ThemeKind {
        if cache::terminal_native_locked() {
            return cache::current_kind();
        }
        let effective = Self::clamp_to_terminal(kind);
        cache::set(effective);
        apply_cursor_color();
        effective
    }

    /// Clamp a theme kind to what the terminal supports.
    fn clamp_to_terminal(kind: ThemeKind) -> ThemeKind {
        if kind.requires_truecolor() && !color_support::detect().has_truecolor() {
            ThemeKind::EchoAgentNight
        } else {
            kind
        }
    }

    /// Push structural colors further from `bg_base` so they survive
    /// Windows display gamma (especially non-HiDPI panels) where the
    /// theme's native ~12-unit RGB steps collapse visually.
    ///
    /// Per-field push amounts are tuned for ConHost specifically —
    /// without colorimetric calibration it takes ~24–32 levels per
    /// channel before mid-gray distinctions read at all.
    ///
    /// The user-prompt block bg is the one asymmetric field: a dark
    /// step on a light canvas "weighs" far more than the symmetric
    /// light step on a dark canvas does, so we push much less in that
    /// direction to stay close to the theme's native RGB on light.
    fn windows_contrast_boost(self, dark: bool) -> Self {
        use ratatui::style::Color;

        /// Move `color` `amount` levels per channel further from `base`.
        /// Returns `color` unchanged when either side isn't RGB.
        fn push_away(base: Color, color: Color, amount: i16) -> Color {
            let Color::Rgb(br, b_green, bb) = base else {
                return color;
            };
            let Color::Rgb(cr, cg, cb) = color else {
                return color;
            };
            let base_lum = br as i16 + b_green as i16 + bb as i16;
            let color_lum = cr as i16 + cg as i16 + cb as i16;
            let sign: i16 = if color_lum >= base_lum { 1 } else { -1 };
            let nudge = |c: u8| (c as i16 + sign * amount).clamp(0, 255) as u8;
            Color::Rgb(nudge(cr), nudge(cg), nudge(cb))
        }

        let bg = self.bg_base;
        let user_block_push = if dark { 28 } else { 8 };
        Self {
            bg_light: push_away(bg, self.bg_light, user_block_push),
            bg_dark: push_away(bg, self.bg_dark, 16),
            bg_highlight: push_away(bg, self.bg_highlight, 28),
            bg_hover: push_away(bg, self.bg_hover, 16),
            gray_dim: push_away(bg, self.gray_dim, 40),
            selection_border: push_away(bg, self.selection_border, 36),
            prompt_border: push_away(bg, self.prompt_border, 40),
            prompt_border_active: push_away(bg, self.prompt_border_active, 60),
            hover_border: push_away(bg, self.hover_border, 28),
            scrollbar_bg: push_away(bg, self.scrollbar_bg, 16),
            scrollbar_fg: push_away(bg, self.scrollbar_fg, 32),
            bg_visual: push_away(bg, self.bg_visual, 16),
            md_code_bg: push_away(bg, self.md_code_bg, 16),
            ..self
        }
    }

    /// Style for shell command suggestion ghost text (dimmed italic).
    pub fn ghost_text_style(&self) -> ratatui::style::Style {
        ratatui::style::Style::default()
            .fg(self.gray_dim)
            .add_modifier(ratatui::style::Modifier::ITALIC)
    }

    /// True when `bg_base` reads as dark per BT.709 luminance. Must be
    /// called pre-quantization while `bg_base` is still RGB; named/Reset
    /// fall back to "dark" (the default theme polarity).
    pub fn is_dark(&self) -> bool {
        use ratatui::style::Color;
        let (r, g, b) = match self.bg_base {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(n) => crate::render::color::indexed_to_rgb(n),
            _ => return true,
        };
        crate::theme::osc11::classify_luminance(r, g, b)
            == crate::theme::system_appearance::SystemAppearance::Dark
    }

    /// Pin chrome and semantic-accent colors to ANSI-named entries so
    /// they survive 16-color quantization. `dark` flips polarity along
    /// two axes:
    ///
    /// 1. **Chrome (bg/borders/scrollbar/text).** ANSI16 has only two
    ///    grays (DarkGray, Gray = silver) plus Black/White, so chrome
    ///    pins to those four to preserve the bg/border hierarchy that
    ///    the theme defines via subtle gray steps.
    /// 2. **Semantic accents (running/error/success/etc.).** Naive
    ///    RGB-distance quantization collapses pastel theme hues onto
    ///    the gray ramp (audit: 18/27 fields became DarkGray or silver
    ///    on echo_agent_night, erasing every state signal). We pin each
    ///    semantic field to a hue-preserving ANSI16 slot, polarity-aware:
    ///    bright variants (`Light*`, idx 9–15) on a dark canvas; normal
    ///    variants (idx 1–7, ~50% luminance) on a light canvas. This
    ///    drops sub-hue differentiation that ANSI16 cannot represent
    ///    (e.g., teal and cyan both pin to `Cyan` / `LightCyan`) but
    ///    guarantees error reads red, success reads green, etc.
    ///
    /// Applied both on legacy Windows ConHost and on any terminal that
    /// advertises only [`color_support::ColorLevel::Basic`].
    ///
    /// Surfaces that want to blend with the body canvas (sunken code
    /// blocks, scrollbar track, paste chip bg) are pinned to the
    /// theme's polarity (Black for dark themes, White for light themes)
    /// instead of `Color::Reset` — using `Reset` would defer to the
    /// user's terminal profile, which can disagree with the selected
    /// theme (e.g., EchoAgentNight on a white terminal would show white
    /// "holes" through every sunken surface).
    fn ansi16_chrome_overrides(self, dark: bool) -> Self {
        use ratatui::style::Color;
        // Theme polarity canvas — what the body bg should look like.
        // Matches the natural quantize result for `bg_base` on both
        // built-in themes, and pins it explicitly so themes whose bg
        // RGB doesn't quantize cleanly still get the right polarity.
        let canvas_bg = if dark { Color::Black } else { Color::White };
        // One palette step off the canvas: DarkGray (ANSI 8) on black,
        // Gray (ANSI 7 / silver) on white. ANSI16 has no slot between
        // these and the canvas, so the elevation reads louder than the
        // truecolor design — but it's guaranteed visible on every
        // 16-color terminal, including museum-grade `TERM=ansi` boxes.
        let elevated_bg = if dark { Color::DarkGray } else { Color::Gray };
        // Max-contrast fg for focused chrome and assistant body.
        let high_contrast_fg = if dark { Color::White } else { Color::Black };
        // Mid-tone fg for muted labels — silver on black, dark-gray on
        // white. This is the higher-contrast of the two muted slots, used
        // for secondary text that still needs to read clearly.
        let muted_fg = if dark { Color::Gray } else { Color::DarkGray };
        // Low-contrast fg for "truly dim" surfaces — sits one step closer
        // to the canvas than `muted_fg`. Used for soft modal/picker frames
        // and the unselected `>` prompt indicator. The polarity is the
        // INVERSE of `muted_fg`: DarkGray sits next to Black, Gray (silver)
        // sits next to White. Distinct from `muted_fg` so dim chrome
        // doesn't read at the same weight as secondary text.
        let dim_fg = if dark { Color::DarkGray } else { Color::Gray };

        // ── Polarity-aware semantic hues ────────────────────────────
        // Normal ANSI hues (idx 1–7) are designed at ~50% luminance and
        // read well on light backgrounds. Light variants (idx 9–15) are
        // full saturation and read well on dark backgrounds. Pinning by
        // polarity restores the chromatic signal that naive nearest-RGB
        // quantization erases when pastel theme RGBs collapse onto the
        // gray ramp.
        let red = if dark { Color::LightRed } else { Color::Red };
        let green = if dark {
            Color::LightGreen
        } else {
            Color::Green
        };
        let yellow = if dark {
            Color::LightYellow
        } else {
            Color::Yellow
        };
        let blue = if dark { Color::LightBlue } else { Color::Blue };
        let magenta = if dark {
            Color::LightMagenta
        } else {
            Color::Magenta
        };
        let cyan = if dark { Color::LightCyan } else { Color::Cyan };
        Self {
            // ── Elevated surfaces: one step off the canvas ──────────────
            // Hover/highlight/visual-selection rows need to read as a
            // distinct "raised" band against the body. Without this every
            // EchoAgentNight bg field quantizes to Color::Black and these
            // become invisible.
            bg_light: elevated_bg,
            bg_highlight: elevated_bg,
            bg_hover: elevated_bg,
            bg_visual: elevated_bg,

            // ── Canvas-matching surfaces ────────────────────────────────
            // Pin to the theme's polarity, NOT Color::Reset. The truecolor
            // "subtle sunken / code block" effect can't be replicated in
            // 16-color, but using the theme polarity guarantees these
            // blend cleanly with `bg_base` regardless of what the user's
            // terminal canvas is set to.
            bg_dark: canvas_bg,
            md_code_bg: canvas_bg,
            paste_bg: canvas_bg,
            scrollbar_bg: canvas_bg,

            // ── Borders: muted (idle prompt) → muted (selection) → high-contrast (active) ──
            // The four-tier truecolor border hierarchy collapses onto
            // three ANSI16 slots:
            //   - `prompt_border` (idle text-input frame) → `muted_fg`,
            //     not `dim_fg`. DarkGray (ANSI 8) is tuned near-bg on
            //     many palettes, so a dim idle frame vanishes the same
            //     way table chrome did (GB-3759).
            //   - `hover_border` (transient mouse-hover) → `DarkGray`,
            //     stable across both polarities so a hover band reads
            //     consistently.
            //   - `selection_border` (sticky selection) → `muted_fg`,
            //     one tier louder than dim, drawing the eye without
            //     screaming.
            //   - `prompt_border_active` (focused) → `high_contrast_fg`,
            //     maximum contrast so focus always pops.
            prompt_border: muted_fg,
            prompt_border_active: high_contrast_fg,
            selection_border: muted_fg,
            hover_border: Color::DarkGray,

            // Scrollbar thumb stays visible against the canvas-matched track.
            scrollbar_fg: muted_fg,

            // ── Foreground / text hierarchy ─────────────────────────────
            // Prompt textarea + chrome captions use these directly (and
            // blend_color cannot mix named ANSI, so they must already
            // be readable slots).
            text_primary: high_contrast_fg,
            text_secondary: muted_fg,
            md_text: high_contrast_fg,
            // Selected user-prompt `>` (drives the user selection accent
            // and the OSC 12 cursor color) takes max-contrast fg so the
            // selection pops against the canvas — White on dark,
            // Black on light.
            accent_user: high_contrast_fg,
            // Two-tier grey: `gray`/`gray_bright` carry secondary text
            // and need readable contrast, so they take `muted_fg` (the
            // higher-contrast slot — silver on black, charcoal on
            // white). `gray_dim` is for genuinely faded chrome (modal
            // frames, unselected `>` prompt indicator) and takes
            // `dim_fg` (the lower-contrast slot — DarkGray on black,
            // silver on white). Two tiers is the most ANSI16 can
            // express without colliding with the elevated-bg slot.
            gray: muted_fg,
            gray_bright: muted_fg,
            gray_dim: dim_fg,

            // ── Semantic accents: polarity-aware hue pins ───────────────
            // State signals (running / completed / error) and content
            // categories (system / skill / etc.) get
            // pinned to a hue that survives ANSI16 instead of collapsing
            // to a gray. ANSI16 only has 6 chromatic slots (no orange,
            // no teal, no violet), so several truecolor accents
            // intentionally fold onto the same slot here — the goal is
            // "preserve the dominant hue family", not "preserve every
            // sub-hue".
            //
            // Magenta family (assistant turn, mid-stream thinking,
            // running indicator, context-overhead accent). All four use
            // a purple/violet hue in both built-in themes.
            accent_assistant: magenta,
            accent_thinking: magenta,
            accent_running: magenta,
            accent_verify: magenta,
            // Red family — error states and diff deletes.
            accent_error: red,
            diff_delete_fg: red,
            // Green family — success states, remember mode, diff inserts.
            accent_success: green,
            accent_remember: green,
            diff_insert_fg: green,
            // Blue family — system messages, skill invocations, fuzzy
            // search matches.
            accent_system: blue,
            accent_skill: blue,
            fuzzy_accent: blue,
            // Cyan family: model name and the legacy `running` indicator (distinct from the magenta `accent_running` used for subagents).
            // ANSI16 has no separate teal slot, so the truecolor teal model accent folds onto cyan here.
            accent_model: cyan,
            running: cyan,
            // Yellow family — warning text, plan-mode gold, shell
            // commands, file paths. ANSI16 has no orange or gold slot,
            // so warm accents all fold onto yellow.
            command: yellow,
            warning: yellow,
            path: yellow,
            accent_plan: yellow,

            // Markdown content: naive Basic quantize lands EchoAgentNight
            // md_code / md_muted / h4–h6 on DarkGray (ANSI 8). Many
            // palettes tune that slot near the background, so inline
            // code and table borders vanish over ssh+tmux (GB-3759).
            // `md_muted` uses muted_fg, not dim_fg — format_table also
            // stacks DIM on table borders.
            md_heading_h1: cyan,
            md_heading_h2: blue,
            md_heading_h3: magenta,
            md_heading_h4: high_contrast_fg,
            md_heading_h5: muted_fg,
            md_heading_h6: muted_fg,
            md_code: cyan,
            md_muted: muted_fg,
            md_task_checked: green,
            md_task_unchecked: muted_fg,
            link_fg: blue,
            diff_equal_fg: muted_fg,
            diff_gutter_fg: muted_fg,
            ..self
        }
    }
}

/// Set the terminal cursor color to the current theme's `accent_user` via OSC 12.
///
/// `Theme::current()` quantizes to the terminal's color level, so on
/// non-truecolor terminals `accent_user` may be `Color::Indexed` or a
/// named ANSI variant. OSC 12 accepts an RGB triple regardless of the
/// terminal's normal SGR color depth, so we resolve every variant back
/// to RGB via [`crate::render::color::resolve_to_rgb`]. Reset (when
/// `NO_COLOR` is set) yields `None` — we skip emission entirely so the
/// terminal keeps its profile-defined cursor color.
///
/// Escape sequence: `\x1b]12;rgb:RR/GG/BB\x07`.
pub fn apply_cursor_color() {
    use std::io::Write;
    let theme = Theme::current();
    let Some((r, g, b)) = crate::render::color::resolve_to_rgb(theme.accent_user) else {
        return;
    };
    echo_agent_shared::stderr::with_locked_stderr(|stderr| {
        let _ = write!(stderr, "\x1b]12;rgb:{r:02x}/{g:02x}/{b:02x}\x07");
        let _ = stderr.flush();
    });
}

/// Reset the terminal cursor color to the terminal's default via OSC 112.
///
/// Called on shutdown to restore the user's original cursor appearance.
pub fn reset_cursor_color() {
    use std::io::Write;
    echo_agent_shared::stderr::with_locked_stderr(|stderr| {
        let _ = write!(stderr, "\x1b]112\x07");
        let _ = stderr.flush();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_auto() {
        assert_eq!(ThemeKind::from_name("auto"), Some(ThemeKind::Auto));
    }

    #[test]
    fn from_name_system() {
        assert_eq!(ThemeKind::from_name("system"), Some(ThemeKind::Auto));
    }

    #[test]
    fn from_name_auto_case_insensitive() {
        assert_eq!(ThemeKind::from_name("AUTO"), Some(ThemeKind::Auto));
        assert_eq!(ThemeKind::from_name("Auto"), Some(ThemeKind::Auto));
        assert_eq!(ThemeKind::from_name("SYSTEM"), Some(ThemeKind::Auto));
    }

    #[test]
    fn display_name_auto() {
        assert_eq!(ThemeKind::Auto.display_name(), "auto");
    }

    #[test]
    fn is_auto_returns_true_for_auto() {
        assert!(ThemeKind::Auto.is_auto());
    }

    #[test]
    fn is_auto_returns_false_for_concrete_variants() {
        assert!(!ThemeKind::EchoAgentNight.is_auto());
        assert!(!ThemeKind::EchoAgentDay.is_auto());
        assert!(!ThemeKind::TokyoNight.is_auto());
        assert!(!ThemeKind::RosePineMoon.is_auto());
        assert!(!ThemeKind::OscuraMidnight.is_auto());
    }

    #[test]
    fn all_excludes_auto() {
        assert!(!ThemeKind::ALL.contains(&ThemeKind::Auto));
    }

    #[test]
    fn available_excludes_auto() {
        assert!(!ThemeKind::available().contains(&ThemeKind::Auto));
    }

    #[test]
    fn is_dark_classifies_built_in_themes() {
        // Sanity-check the polarity sampler against the theme catalog.
        assert!(Theme::echo_agent_night().is_dark());
        assert!(Theme::tokyonight().is_dark());
        assert!(Theme::rosepine_moon().is_dark());
        assert!(Theme::oscura_midnight().is_dark());
        assert!(!Theme::echo_agent_day().is_dark());
    }

    #[test]
    fn ansi16_overrides_dark_uses_bright_white_high_contrast() {
        use ratatui::style::Color;
        let t = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t.bg_light, Color::DarkGray);
        assert_eq!(t.bg_highlight, Color::DarkGray);
        // Idle prompt border sits at `muted_fg` (Gray on dark canvas);
        // focused border jumps to max-contrast White.
        assert_eq!(t.prompt_border, Color::Gray);
        assert_eq!(t.prompt_border_active, Color::White);
        assert_eq!(t.md_text, Color::White);
        assert_eq!(t.text_primary, Color::White);
        assert_eq!(t.text_secondary, Color::Gray);
        // Two-tier grey: secondary text (`gray`) reads at the muted
        // slot (silver), `gray_dim` reads at the dim slot (DarkGray) —
        // see `ansi16_overrides_gray_hierarchy_collapses_to_two_slots`.
        assert_eq!(t.gray, Color::Gray);
        assert_eq!(t.gray_dim, Color::DarkGray);
    }

    #[test]
    fn ansi16_overrides_light_inverts_high_contrast_and_elevated_bg() {
        // Light canvas inverts polarity: elevated bg reads darker
        // (silver step from white), high-contrast fg is Black, muted
        // fg is DarkGray, and the dim slot (`gray_dim`) flips to silver
        // — see `ansi16_overrides_gray_hierarchy_collapses_to_two_slots`.
        use ratatui::style::Color;
        let t = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t.bg_light, Color::Gray);
        assert_eq!(t.bg_highlight, Color::Gray);
        assert_eq!(t.prompt_border, Color::DarkGray);
        assert_eq!(t.prompt_border_active, Color::Black);
        assert_eq!(t.md_text, Color::Black);
        assert_eq!(t.text_primary, Color::Black);
        assert_eq!(t.text_secondary, Color::DarkGray);
        assert_eq!(t.gray, Color::DarkGray);
        assert_eq!(t.gray_dim, Color::Gray);
    }

    #[test]
    fn ansi16_overrides_preserve_bg_base() {
        // `bg_base` belongs to the user's terminal session, not to us —
        // we never overwrite it. Polarity-pinned canvas surfaces
        // (`bg_dark`, `md_code_bg`, `paste_bg`, `scrollbar_bg`) are
        // tested separately in
        // `ansi16_overrides_canvas_matching_surfaces_use_theme_polarity`.
        let base = Theme::echo_agent_night();
        let t = base.ansi16_chrome_overrides(true);
        assert_eq!(t.bg_base, base.bg_base);
    }

    #[test]
    fn ansi16_overrides_state_accents_pin_to_polarity_aware_hue() {
        // running / completed / error must read as their hue family
        // even at ANSI16. Dark canvas → bright (Light*) variants, light
        // canvas → normal variants. Without these pins the source
        // pastel RGBs collapse onto silver/DarkGray and every state
        // signal becomes the same gray.
        use ratatui::style::Color;
        let t_dark = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t_dark.accent_error, Color::LightRed);
        assert_eq!(t_dark.accent_success, Color::LightGreen);
        assert_eq!(t_dark.accent_running, Color::LightMagenta);

        let t_light = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t_light.accent_error, Color::Red);
        assert_eq!(t_light.accent_success, Color::Green);
        assert_eq!(t_light.accent_running, Color::Magenta);
    }

    #[test]
    fn ansi16_overrides_md_palette_never_lands_on_dark_gray() {
        // GB-3759: on a dark canvas no md field may land on DarkGray,
        // the slot palettes tune near their background.
        use ratatui::style::Color;
        let t = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        for (name, c) in [
            ("md_heading_h1", t.md_heading_h1),
            ("md_heading_h2", t.md_heading_h2),
            ("md_heading_h3", t.md_heading_h3),
            ("md_heading_h4", t.md_heading_h4),
            ("md_heading_h5", t.md_heading_h5),
            ("md_heading_h6", t.md_heading_h6),
            ("md_code", t.md_code),
            ("md_muted", t.md_muted),
            ("md_task_checked", t.md_task_checked),
            ("md_task_unchecked", t.md_task_unchecked),
            ("link_fg", t.link_fg),
            ("diff_equal_fg", t.diff_equal_fg),
            ("diff_gutter_fg", t.diff_gutter_fg),
        ] {
            assert_ne!(
                c,
                Color::DarkGray,
                "{name} must not land on DarkGray on a dark canvas \
                 (invisible on palettes that tune ANSI 8 near-bg)"
            );
            assert!(
                !matches!(c, Color::Rgb(..) | Color::Indexed(_)),
                "{name} must be a named ANSI16 color, got {c:?}"
            );
        }
    }

    #[test]
    fn ansi16_overrides_md_palette_polarity_aware_hues() {
        use ratatui::style::Color;
        let t_dark = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t_dark.md_code, Color::LightCyan);
        assert_eq!(t_dark.md_muted, Color::Gray);
        assert_eq!(t_dark.md_heading_h1, Color::LightCyan);
        assert_eq!(t_dark.md_heading_h2, Color::LightBlue);
        assert_eq!(t_dark.md_heading_h3, Color::LightMagenta);
        assert_eq!(t_dark.md_heading_h4, Color::White);
        assert_eq!(t_dark.link_fg, Color::LightBlue);
        assert_eq!(t_dark.md_task_checked, Color::LightGreen);

        let t_light = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t_light.md_code, Color::Cyan);
        assert_eq!(t_light.md_muted, Color::DarkGray);
        assert_eq!(t_light.md_heading_h1, Color::Cyan);
        assert_eq!(t_light.md_heading_h2, Color::Blue);
        assert_eq!(t_light.md_heading_h3, Color::Magenta);
        assert_eq!(t_light.md_heading_h4, Color::Black);
        assert_eq!(t_light.link_fg, Color::Blue);
        assert_eq!(t_light.md_task_checked, Color::Green);
    }

    #[test]
    fn ansi16_overrides_magenta_family_shares_slot() {
        // assistant turn, mid-stream thinking, running indicator, and
        // context-overhead accent all use a purple/violet hue in truecolor.
        // ANSI16 has one magenta slot per polarity, so they all fold
        // onto it together — they live in different surfaces so the
        // collision doesn't cause confusion.
        use ratatui::style::Color;
        let t = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t.accent_assistant, Color::LightMagenta);
        assert_eq!(t.accent_thinking, Color::LightMagenta);
        assert_eq!(t.accent_running, Color::LightMagenta);
        assert_eq!(t.accent_verify, Color::LightMagenta);
    }

    #[test]
    fn ansi16_overrides_yellow_family_absorbs_orange_and_gold() {
        // ANSI16 has no orange or gold slot, so warm accents (command,
        // warning, path, plan) all fold onto Yellow /
        // LightYellow. This is intentional: preserving the "warm"
        // semantic is more important than per-accent differentiation
        // that the palette cannot represent.
        use ratatui::style::Color;
        let t_dark = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        for f in [
            t_dark.command,
            t_dark.warning,
            t_dark.path,
            t_dark.accent_plan,
        ] {
            assert_eq!(f, Color::LightYellow);
        }

        let t_light = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        for f in [
            t_light.command,
            t_light.warning,
            t_light.path,
            t_light.accent_plan,
        ] {
            assert_eq!(f, Color::Yellow);
        }
    }

    #[test]
    fn ansi16_overrides_cyan_family_absorbs_teal() {
        // ANSI16 has no teal slot; the model teal folds onto cyan.
        // The `running` indicator (legacy cyan, distinct from the magenta `accent_running` used for subagents) also lives here.
        use ratatui::style::Color;
        let t = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t.accent_model, Color::LightCyan);
        assert_eq!(t.running, Color::LightCyan);
    }

    #[test]
    fn ansi16_overrides_blue_family_pins_system_skill_fuzzy() {
        // System messages, skill invocations, and fuzzy-search matches
        // all carry the same blue family in truecolor.
        use ratatui::style::Color;
        let t_dark = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t_dark.accent_system, Color::LightBlue);
        assert_eq!(t_dark.accent_skill, Color::LightBlue);
        assert_eq!(t_dark.fuzzy_accent, Color::LightBlue);

        let t_light = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t_light.accent_system, Color::Blue);
        assert_eq!(t_light.accent_skill, Color::Blue);
        assert_eq!(t_light.fuzzy_accent, Color::Blue);
    }

    #[test]
    fn ansi16_overrides_diff_fg_uses_polarity_aware_red_green() {
        // Diff add/remove rely on fg color for their primary signal at
        // ANSI16 (the subtle pastel bg tints don't survive quantization).
        // Pin fg to red / green so deletes and inserts stay legible.
        use ratatui::style::Color;
        let t_dark = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t_dark.diff_delete_fg, Color::LightRed);
        assert_eq!(t_dark.diff_insert_fg, Color::LightGreen);

        let t_light = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t_light.diff_delete_fg, Color::Red);
        assert_eq!(t_light.diff_insert_fg, Color::Green);
    }

    #[test]
    fn ansi16_overrides_accent_user_uses_high_contrast() {
        // accent_user drives the selected-user-prompt `>` color and the
        // OSC 12 cursor color. It's pinned to max-contrast fg in both
        // polarities so the selection always pops against the canvas:
        //   - dark canvas → Color::White
        //   - light canvas → Color::Black
        use ratatui::style::Color;
        let t_dark = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t_dark.accent_user, Color::White);

        let t_light = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t_light.accent_user, Color::Black);
    }

    #[test]
    fn ansi16_overrides_extended_dark_pins_elevated_bg_to_dark_gray() {
        // Without these pins, every dark RGB bg quantizes to Color::Black
        // and the hover/visual/highlight bands collapse onto the canvas.
        use ratatui::style::Color;
        let t = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t.bg_hover, Color::DarkGray);
        assert_eq!(t.bg_visual, Color::DarkGray);
    }

    #[test]
    fn ansi16_overrides_extended_light_pins_elevated_bg_to_gray() {
        use ratatui::style::Color;
        let t = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t.bg_hover, Color::Gray);
        assert_eq!(t.bg_visual, Color::Gray);
    }

    #[test]
    fn ansi16_overrides_canvas_matching_surfaces_use_theme_polarity() {
        // Sunken bg, code-block bg, paste chip bg, and scrollbar track
        // must match the theme polarity (Black for dark themes, White
        // for light) — NOT Color::Reset, which would defer to the
        // user's terminal canvas and create polarity mismatches when
        // the theme disagrees with the terminal profile (e.g. EchoAgentNight
        // running on a white-canvas terminal).
        use ratatui::style::Color;
        let t_dark = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t_dark.bg_dark, Color::Black);
        assert_eq!(t_dark.md_code_bg, Color::Black);
        assert_eq!(t_dark.paste_bg, Color::Black);
        assert_eq!(t_dark.scrollbar_bg, Color::Black);

        let t_light = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t_light.bg_dark, Color::White);
        assert_eq!(t_light.md_code_bg, Color::White);
        assert_eq!(t_light.paste_bg, Color::White);
        assert_eq!(t_light.scrollbar_bg, Color::White);
    }

    #[test]
    fn ansi16_overrides_border_hierarchy_is_distinct() {
        // Border hierarchy:
        //   prompt_border (muted, idle) → muted (selection) → high-contrast (focused)
        // On dark canvas, idle prompt and selection share `Gray` (silver)
        // so the frame survives palettes that tune ANSI 8 near-bg;
        // `hover_border` stays `DarkGray`, `prompt_border_active` is `White`.
        // On light canvas, `prompt_border` / `selection_border` /
        // `hover_border` all sit on `DarkGray`; focus is `Black`.
        use ratatui::style::Color;
        let t_dark = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t_dark.hover_border, Color::DarkGray);
        assert_eq!(t_dark.prompt_border, Color::Gray);
        assert_eq!(t_dark.selection_border, Color::Gray);
        assert_eq!(t_dark.prompt_border_active, Color::White);
        assert_ne!(t_dark.selection_border, t_dark.prompt_border_active);

        let t_light = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t_light.hover_border, Color::DarkGray);
        assert_eq!(t_light.prompt_border, Color::DarkGray);
        assert_eq!(t_light.selection_border, Color::DarkGray);
        assert_eq!(t_light.prompt_border_active, Color::Black);
        assert_ne!(t_light.selection_border, t_light.prompt_border_active);
    }

    #[test]
    fn ansi16_overrides_scrollbar_thumb_visible_against_canvas() {
        // scrollbar_fg must not equal scrollbar_bg or the thumb is
        // invisible against the canvas-pinned track.
        use ratatui::style::Color;
        let t_dark = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t_dark.scrollbar_fg, Color::Gray);
        assert_ne!(t_dark.scrollbar_fg, t_dark.scrollbar_bg);

        let t_light = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t_light.scrollbar_fg, Color::DarkGray);
        assert_ne!(t_light.scrollbar_fg, t_light.scrollbar_bg);
    }

    #[test]
    fn scrollbar_thumb_contrasts_with_track_in_all_themes() {
        // Regression test for oscura-midnight: its thumb
        // (`ELEVATED`, Σrgb 55) was *darker* than its track
        // (`HIGHLIGHT_LOW`, Σrgb 62), so the scrollbar was invisible.
        // Follow mode makes this stricter: the main-chat scrollbar blends
        // the thumb 40% toward the track (`views/agent.rs`), so the
        // full-strength delta must be comfortably visible to survive it.
        //
        // Requirements per theme, in native truecolor RGB:
        //   1. Polarity: thumb sits *away* from the canvas relative to the
        //      track — lighter on dark themes, darker on light themes.
        //   2. Magnitude: ≥ 30 summed-RGB units between thumb and track
        //      (the smallest shipping delta, tokyonight, is 34).
        use ratatui::style::Color;
        let lum = |c: Color, field: &str, kind: ThemeKind| -> i32 {
            let Color::Rgb(r, g, b) = c else {
                panic!("{kind:?} {field} must be Color::Rgb, got {c:?}");
            };
            r as i32 + g as i32 + b as i32
        };
        for &kind in ThemeKind::ALL {
            let theme = match kind {
                ThemeKind::EchoAgentNight => Theme::echo_agent_night(),
                ThemeKind::EchoAgentDay => Theme::echo_agent_day(),
                ThemeKind::TokyoNight => Theme::tokyonight(),
                ThemeKind::RosePineMoon => Theme::rosepine_moon(),
                ThemeKind::OscuraMidnight => Theme::oscura_midnight(),
                ThemeKind::Auto => unreachable!("ALL excludes Auto"),
            };
            let track = lum(theme.scrollbar_bg, "scrollbar_bg", kind);
            let thumb = lum(theme.scrollbar_fg, "scrollbar_fg", kind);
            let delta = thumb - track;
            if theme.is_dark() {
                assert!(
                    delta >= 30,
                    "{kind:?}: thumb (Σ{thumb}) must be ≥30 lighter than \
                     track (Σ{track}) on a dark theme, got Δ{delta}"
                );
            } else {
                assert!(
                    delta <= -30,
                    "{kind:?}: thumb (Σ{thumb}) must be ≥30 darker than \
                     track (Σ{track}) on a light theme, got Δ{delta}"
                );
            }
        }
    }

    #[test]
    fn ansi16_quantize_without_override_collapses_echo_agentnight_backgrounds() {
        // Regression-ratchet for the override gate in `Theme::current`:
        // naive `Basic` quantization maps every dark EchoAgentNight bg field
        // to `Color::Black`, erasing the hierarchy. This test exists to
        // make the motivation for `ansi16_chrome_overrides` explicit —
        // if quantization later gains a "dark gray" mid-tone (e.g., via
        // an ANSI24/ANSI32 level), this test will start failing and the
        // override scope should be revisited.
        use ratatui::style::Color;
        let q = Theme::echo_agent_night().quantized(color_support::ColorLevel::Basic);
        for (name, color) in [
            ("bg_base", q.bg_base),
            ("bg_light", q.bg_light),
            ("bg_dark", q.bg_dark),
            ("bg_highlight", q.bg_highlight),
            ("bg_hover", q.bg_hover),
            ("bg_visual", q.bg_visual),
            ("md_code_bg", q.md_code_bg),
            ("scrollbar_bg", q.scrollbar_bg),
        ] {
            assert_eq!(
                color,
                Color::Black,
                "{name} should collapse to Black without the override"
            );
        }
    }

    #[test]
    fn ansi16_overrides_gray_hierarchy_collapses_to_two_slots() {
        // Basic exposes only DarkGray + Gray as named greys (Black and
        // White are reserved for canvas / max-contrast fg). We split
        // the dim/medium/bright triplet into two tiers:
        //   - `gray`, `gray_bright` → `muted_fg` (the higher-contrast
        //     slot — silver on Black, charcoal on White). Secondary
        //     text needs to stay readable.
        //   - `gray_dim` → `dim_fg` (the lower-contrast slot, sitting
        //     one tier closer to the canvas — DarkGray on Black, silver
        //     on White). Used for genuinely faded chrome (modal frames,
        //     unselected `>` prompt indicator).
        // ANSI16 has only two grey slots so we can't get a true
        // three-tier hierarchy; collapsing bright+medium onto the
        // brighter slot keeps secondary text legible while still
        // separating "dim" from "muted".
        use ratatui::style::Color;
        let t_dark = Theme::echo_agent_night().ansi16_chrome_overrides(true);
        assert_eq!(t_dark.gray, Color::Gray);
        assert_eq!(t_dark.gray_bright, Color::Gray);
        assert_eq!(t_dark.gray_dim, Color::DarkGray);
        assert_ne!(t_dark.gray, t_dark.gray_dim);

        let t_light = Theme::echo_agent_day().ansi16_chrome_overrides(false);
        assert_eq!(t_light.gray, Color::DarkGray);
        assert_eq!(t_light.gray_bright, Color::DarkGray);
        assert_eq!(t_light.gray_dim, Color::Gray);
        assert_ne!(t_light.gray, t_light.gray_dim);
    }

    #[test]
    fn auto_does_not_require_truecolor() {
        assert!(!ThemeKind::Auto.requires_truecolor());
    }

    #[test]
    fn resolve_to_rgb_handles_rgb_indexed_named_and_reset() {
        use crate::render::color::{indexed_to_rgb, resolve_to_rgb};
        use ratatui::style::Color;
        // Truecolor pass-through.
        assert_eq!(resolve_to_rgb(Color::Rgb(12, 34, 56)), Some((12, 34, 56)));
        // Indexed routes through indexed_to_rgb (16 = (0, 0, 0) — first cube cell).
        assert_eq!(resolve_to_rgb(Color::Indexed(16)), Some(indexed_to_rgb(16)));
        // Each named ANSI variant resolves to indexed_to_rgb(0..=15).
        let named = [
            (Color::Black, 0u8),
            (Color::Red, 1),
            (Color::Green, 2),
            (Color::Yellow, 3),
            (Color::Blue, 4),
            (Color::Magenta, 5),
            (Color::Cyan, 6),
            (Color::Gray, 7),
            (Color::DarkGray, 8),
            (Color::LightRed, 9),
            (Color::LightGreen, 10),
            (Color::LightYellow, 11),
            (Color::LightBlue, 12),
            (Color::LightMagenta, 13),
            (Color::LightCyan, 14),
            (Color::White, 15),
        ];
        for (color, idx) in named {
            assert_eq!(
                resolve_to_rgb(color),
                Some(indexed_to_rgb(idx)),
                "named variant {color:?} should map to indexed_to_rgb({idx})"
            );
        }
        // Reset is the only no-op.
        assert_eq!(resolve_to_rgb(Color::Reset), None);
    }

    #[test]
    fn from_name_concrete_variants_still_work() {
        assert_eq!(
            ThemeKind::from_name("echo-agent-night"),
            Some(ThemeKind::EchoAgentNight)
        );
        assert_eq!(
            ThemeKind::from_name("dark"),
            Some(ThemeKind::EchoAgentNight)
        );
        assert_eq!(
            ThemeKind::from_name("echo-agent-day"),
            Some(ThemeKind::EchoAgentDay)
        );
        assert_eq!(ThemeKind::from_name("light"), Some(ThemeKind::EchoAgentDay));
        assert_eq!(
            ThemeKind::from_name("tokyonight"),
            Some(ThemeKind::TokyoNight)
        );
        assert_eq!(
            ThemeKind::from_name("rosepine"),
            Some(ThemeKind::RosePineMoon)
        );
        assert_eq!(
            ThemeKind::from_name("oscura"),
            Some(ThemeKind::OscuraMidnight)
        );
        assert_eq!(
            ThemeKind::from_name("oscura-midnight"),
            Some(ThemeKind::OscuraMidnight)
        );
    }

    /// `FromStr` agrees with `from_name` for all canonicals + aliases.
    #[test]
    fn from_str_matches_from_name_for_all_canonicals() {
        // Mapping `from_name`'s alias matrix into the `FromStr` API.
        let cases = [
            ("auto", ThemeKind::Auto),
            ("system", ThemeKind::Auto),
            ("echo-agent-night", ThemeKind::EchoAgentNight),
            ("echo-agent-night", ThemeKind::EchoAgentNight),
            ("dark", ThemeKind::EchoAgentNight),
            ("tokyonight", ThemeKind::TokyoNight),
            ("tokyo-night", ThemeKind::TokyoNight),
            ("tokyo", ThemeKind::TokyoNight),
            ("echo-agent-day", ThemeKind::EchoAgentDay),
            ("echo-agent-day", ThemeKind::EchoAgentDay),
            ("light", ThemeKind::EchoAgentDay),
            ("day", ThemeKind::EchoAgentDay),
            ("rosepine", ThemeKind::RosePineMoon),
            ("rose-pine", ThemeKind::RosePineMoon),
            ("rosepine-moon", ThemeKind::RosePineMoon),
            ("rose-pine-moon", ThemeKind::RosePineMoon),
        ];
        for (name, expected) in cases {
            assert_eq!(
                name.parse::<ThemeKind>(),
                Ok(expected),
                "name `{name}` must parse to {expected:?}",
            );
            // Case-insensitive symmetry.
            assert_eq!(
                name.to_uppercase().parse::<ThemeKind>(),
                Ok(expected),
                "name `{name}` (upper) must parse to {expected:?}",
            );
        }
        assert_eq!("nonexistent".parse::<ThemeKind>(), Err(()));
        assert_eq!("".parse::<ThemeKind>(), Err(()));
    }
}
