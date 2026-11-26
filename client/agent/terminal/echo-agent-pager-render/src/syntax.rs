//! Syntax highlighting initialization.
//!
//! Provides lazily-initialized `Syntect` instances for code highlighting.
//! Dark themes (EchoAgentNight, TokyoNight) share `echo-agent-night.tmTheme`;
//! EchoAgentDay uses `echo-agent-day.tmTheme` with deepened colors for light backgrounds.
//!
//! ## Minimal / terminal-native lock
//!
//! While [`crate::theme::cache::terminal_native_locked`] is set, chrome uses
//! [`Theme::terminal_default`](crate::theme::Theme::terminal_default) and
//! `current_kind()` is a nominal `EchoAgentNight` (so leftover kind-keyed paths
//! still resolve). Syntect therefore loads the night `.tmTheme` whose pastel
//! RGB tokens, after naive ANSI-16 quantization, collapse to **White** —
//! invisible on light terminal profiles.
//!
//! Under the lock we do **not** detect light/dark. Instead:
//! 1. Near-gray tokens → `Color::Reset` (terminal default fg; always readable).
//! 2. Chromatic tokens → base ANSI-16 accents (Red/Green/Yellow/Blue/Magenta/Cyan),
//!    never White/Black/bright variants.
//!
//! That matches the "first + second" minimal syntax policy: default-fg baseline
//! plus a dual-polarity accent map, with zero polarity detection.

use std::sync::OnceLock;

pub use echo_agent_markdown::Syntect;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::theme::ThemeKind;

static SYNTECT_ECHO_AGENT_NIGHT: OnceLock<Syntect> = OnceLock::new();
static SYNTECT_TOKYONIGHT: OnceLock<Syntect> = OnceLock::new();
static SYNTECT_ECHO_AGENT_DAY: OnceLock<Syntect> = OnceLock::new();

/// Convert syntect style to ratatui foreground-only style, quantized for
/// terminal color support (or polarity-safe under the terminal-native lock).
pub fn syntect_to_ratatui_fg(style: syntect::highlighting::Style) -> Style {
    let fg = syntect_rgb_to_fg(style.foreground.r, style.foreground.g, style.foreground.b);
    let mut out = Style::default().fg(fg);
    use syntect::highlighting::FontStyle;
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

/// Map a syntect RGB triplet to a ratatui foreground color.
///
/// Under the terminal-native lock, uses [`polarity_safe_syntax_fg`]; otherwise
/// quantizes via the normal theme color pipeline.
pub fn syntect_rgb_to_fg(r: u8, g: u8, b: u8) -> Color {
    if crate::theme::cache::terminal_native_locked() {
        polarity_safe_syntax_fg(r, g, b)
    } else {
        crate::theme::quantize(Color::Rgb(r, g, b))
    }
}

/// Dual-polarity-safe ANSI mapping for syntax tokens on a transparent canvas.
///
/// - Low chroma (gray / near-gray body text) → [`Color::Reset`] so the host
///   default fg carries contrast on both light and dark profiles.
/// - Saturated hues → base ANSI Red/Green/Yellow/Blue/Magenta/Cyan only.
///
/// Never returns White, Black, or bright (Light*) variants — those are the
/// colors that vanish on the opposite polarity after naive RGB→ANSI16.
pub fn polarity_safe_syntax_fg(r: u8, g: u8, b: u8) -> Color {
    let max = r.max(g).max(b) as i32;
    let min = r.min(g).min(b) as i32;
    let chroma = max - min;
    // Night default body (~#c8c8c8) and dim comments are near-gray.
    if chroma < 40 {
        return Color::Reset;
    }
    // Integer HSV hue in degrees [0, 360).
    let (ri, gi, bi) = (r as i32, g as i32, b as i32);
    let h = if max == ri {
        let mut h = (gi - bi) * 60 / chroma;
        if h < 0 {
            h += 360;
        }
        h
    } else if max == gi {
        (bi - ri) * 60 / chroma + 120
    } else {
        (ri - gi) * 60 / chroma + 240
    };
    // Magenta starts at 255° so Tokyo Night purple (#bb9af7, ~261°) lands
    // Magenta rather than Blue; pure blues (~221°) stay Blue.
    match h {
        0..30 | 330..=360 => Color::Red,
        30..90 => Color::Yellow,
        90..150 => Color::Green,
        150..210 => Color::Cyan,
        210..255 => Color::Blue,
        _ => Color::Magenta,
    }
}

/// Highlight a single line of source, falling back to plain text style.
///
/// Under the terminal-native lock, syntect tokens are remapped via
/// [`polarity_safe_syntax_fg`]; if highlighting fails, `fallback` (typically
/// [`Theme::primary`](crate::theme::Theme::primary) = Reset) is used.
pub fn highlight_line(
    text: &str,
    highlighter: &mut Option<syntect::easy::HighlightLines<'_>>,
    syntect: &Syntect,
    fallback: Style,
) -> Vec<Span<'static>> {
    if let Some(hl) = highlighter.as_mut()
        && let Ok(ranges) = hl.highlight_line(&format!("{text}\n"), &syntect.syntax_set)
    {
        let mut spans = Vec::new();
        for (style, segment) in ranges {
            let mut s = segment.to_owned();
            while s.ends_with('\n') || s.ends_with('\r') {
                s.pop();
            }
            if s.is_empty() {
                continue;
            }
            spans.push(Span::styled(s, syntect_to_ratatui_fg(style)));
        }
        if !spans.is_empty() {
            return spans;
        }
    }
    vec![Span::styled(text.to_string(), fallback)]
}

/// Returns the syntect instance matching the active theme.
///
/// Note: while the terminal-native lock is engaged, [`Theme::current_kind`]
/// reports a nominal `EchoAgentNight`, so this returns the night theme. Token
/// colors are remapped in [`syntect_to_ratatui_fg`] — do not load a day
/// theme based on OS/terminal polarity detection.
pub fn get_syntect() -> &'static Syntect {
    match crate::theme::Theme::current_kind() {
        ThemeKind::EchoAgentNight
        | ThemeKind::RosePineMoon
        | ThemeKind::OscuraMidnight
        | ThemeKind::Auto => SYNTECT_ECHO_AGENT_NIGHT
            .get_or_init(|| Syntect::new(include_bytes!("../assets/echo-agent-night.tmTheme"))),
        ThemeKind::TokyoNight => SYNTECT_TOKYONIGHT
            .get_or_init(|| Syntect::new(include_bytes!("../assets/tokyo-night.tmTheme"))),
        ThemeKind::EchoAgentDay => SYNTECT_ECHO_AGENT_DAY
            .get_or_init(|| Syntect::new(include_bytes!("../assets/echo-agent-day.tmTheme"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::cache as theme_cache;

    /// Hold the theme test lock so we can flip the terminal-native flag.
    fn with_native_lock<R>(locked: bool, f: impl FnOnce() -> R) -> R {
        let _guard = theme_cache::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        theme_cache::reset_for_test();
        theme_cache::set_terminal_native_lock(locked);
        let out = f();
        theme_cache::set_terminal_native_lock(false);
        theme_cache::reset_for_test();
        out
    }

    #[test]
    fn polarity_safe_grays_are_reset() {
        // Night default body / comments.
        assert_eq!(polarity_safe_syntax_fg(0xc8, 0xc8, 0xc8), Color::Reset);
        assert_eq!(polarity_safe_syntax_fg(0x6c, 0x6c, 0x6c), Color::Reset);
        assert_eq!(polarity_safe_syntax_fg(0xb2, 0xb2, 0xb2), Color::Reset);
        assert_eq!(polarity_safe_syntax_fg(0x44, 0x44, 0x44), Color::Reset);
    }

    #[test]
    fn polarity_safe_never_emits_white_or_black() {
        // Common night-theme pastels that naive ANSI16 maps to White.
        let samples = [
            (0xbb, 0x9a, 0xf7), // magenta
            (0x7d, 0xcf, 0xff), // cyan
            (0x7a, 0xa2, 0xf7), // blue
            (0xff, 0x9e, 0x64), // orange
            (0xf7, 0x76, 0x8e), // red
            (0xe0, 0xaf, 0x68), // yellow
            (0x9e, 0xce, 0x6a), // green
            (0xc8, 0xc8, 0xc8), // gray body
        ];
        for (r, g, b) in samples {
            let c = polarity_safe_syntax_fg(r, g, b);
            assert!(
                !matches!(
                    c,
                    Color::White
                        | Color::Black
                        | Color::Gray
                        | Color::DarkGray
                        | Color::LightRed
                        | Color::LightGreen
                        | Color::LightYellow
                        | Color::LightBlue
                        | Color::LightMagenta
                        | Color::LightCyan
                ),
                "polarity-unsafe color for #{r:02x}{g:02x}{b:02x}: {c:?}"
            );
        }
    }

    #[test]
    fn polarity_safe_chromatic_buckets() {
        assert_eq!(polarity_safe_syntax_fg(0xf7, 0x76, 0x8e), Color::Red);
        assert_eq!(polarity_safe_syntax_fg(0xe0, 0xaf, 0x68), Color::Yellow);
        assert_eq!(polarity_safe_syntax_fg(0x9e, 0xce, 0x6a), Color::Yellow); // lime → yellow bucket
        assert_eq!(polarity_safe_syntax_fg(0x7d, 0xcf, 0xff), Color::Cyan);
        assert_eq!(polarity_safe_syntax_fg(0x7a, 0xa2, 0xf7), Color::Blue);
        assert_eq!(polarity_safe_syntax_fg(0xbb, 0x9a, 0xf7), Color::Magenta);
    }

    #[test]
    fn syntect_rgb_to_fg_uses_polarity_safe_when_locked() {
        with_native_lock(true, || {
            // Pastel that naive quantize would turn White.
            assert_eq!(syntect_rgb_to_fg(0xc8, 0xc8, 0xc8), Color::Reset);
            assert_eq!(syntect_rgb_to_fg(0xbb, 0x9a, 0xf7), Color::Magenta);
        });
    }

    #[test]
    fn highlight_line_fallback_when_no_highlighter() {
        let syn = get_syntect();
        let mut hl = None;
        let fallback = Style::default().fg(Color::Reset);
        let spans = highlight_line("fn main() {}", &mut hl, syn, fallback);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "fn main() {}");
        assert_eq!(spans[0].style.fg, Some(Color::Reset));
    }

    #[test]
    fn highlight_line_under_native_lock_avoids_white_tokens() {
        with_native_lock(true, || {
            let syn = get_syntect();
            let mut hl = syn.highlight_lines_for_token("rust");
            let fallback = Style::default().fg(Color::Reset);
            let spans = highlight_line("fn main() { let x = 1; /* c */ }", &mut hl, syn, fallback);
            assert!(!spans.is_empty());
            for span in &spans {
                let fg = span.style.fg;
                assert!(
                    !matches!(fg, Some(Color::White)),
                    "token {:?} painted White under native lock",
                    span.content
                );
            }
        });
    }
}
