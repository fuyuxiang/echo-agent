//! Context usage bar — shows token usage in the status bar.
//!
//! Default builds a `Line<'static>` of styled spans: `8.5K / 1.0M` (actual tokens,
//! colored by usage percentage). On hover, replaces the tokens with a progress
//! bar + percentage, e.g. `█████ 42.0%`. The bar width is derived from the
//! default string length so the hover line is the same total width — no layout
//! shift on hover. The default is right-padded to a minimum of 6 columns so the
//! width invariant holds even for degenerate inputs like `0 / 9`.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::progress_bar::progress_bar_spans;
use crate::theme::Theme;

// ---------------------------------------------------------------------------
// Formatting utilities
// ---------------------------------------------------------------------------

/// Format a percentage as a fixed-width 5-char string.
///
/// - `< 10`:  `"X.XX%"` (e.g. `"0.00%"`, `"5.12%"`)
/// - `10–99`: `"XX.X%"` (e.g. `"20.1%"`, `"99.9%"`)
/// - `≥ 100`: `"MAX %"`
pub fn fmt_pct5(pct: f64) -> String {
    if pct >= 100.0 {
        "MAX %".to_string()
    } else if pct < 10.0 {
        format!("{pct:.2}%")
    } else {
        format!("{pct:.1}%")
    }
}

/// Format a token count as a compact string (≤4 chars).
///
/// - `0–999`:     `"0"`, `"12"`, `"999"`
/// - `1K–9.9K`:   `"1.2K"` (4 chars)
/// - `10K–999K`:  `"12K"`, `"999K"` (≤4 chars)
/// - `1M–9.9M`:   `"1.2M"` (4 chars)
/// - `10M+`:      `"12M"`, `"123M"` (≤4 chars)
pub fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else if n < 1_000_000 {
        format!("{}K", n / 1_000)
    } else if n < 10_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else {
        format!("{}M", n / 1_000_000)
    }
}

// ---------------------------------------------------------------------------
// Color blending
// ---------------------------------------------------------------------------

/// A breakpoint for color blending: at `pct` percent, the bar color is `color`.
#[derive(Debug, Clone, Copy)]
pub struct ColorBreakpoint {
    pub pct: f64,
    pub color: Color,
}

/// Default breakpoints: text_primary → accent_user → warning → accent_error.
///
/// Breakpoint colors are raw RGB. The final color produced by [`blend_color`]
/// is quantized by the caller (see [`context_bar_line`]) so the output always
/// matches the terminal's capability level.
pub fn default_breakpoints(theme: &Theme) -> Vec<ColorBreakpoint> {
    vec![
        ColorBreakpoint {
            pct: 0.0,
            color: theme.text_primary,
        },
        ColorBreakpoint {
            pct: 50.0,
            color: theme.accent_user,
        },
        ColorBreakpoint {
            pct: 65.0,
            color: theme.accent_user,
        },
        ColorBreakpoint {
            pct: 75.0,
            color: theme.warning,
        },
        ColorBreakpoint {
            pct: 85.0,
            color: theme.warning,
        },
        ColorBreakpoint {
            pct: 95.0,
            color: theme.accent_error,
        },
    ]
}

/// Blend between breakpoints for a given percentage.
pub fn blend_color(pct: f64, breakpoints: &[ColorBreakpoint]) -> Color {
    if breakpoints.is_empty() {
        return Color::Reset;
    }
    if pct <= breakpoints[0].pct {
        return breakpoints[0].color;
    }
    for i in 1..breakpoints.len() {
        if pct <= breakpoints[i].pct {
            let t = (pct - breakpoints[i - 1].pct) / (breakpoints[i].pct - breakpoints[i - 1].pct);
            return lerp_color(breakpoints[i - 1].color, breakpoints[i].color, t as f32);
        }
    }
    breakpoints.last().unwrap().color
}

/// Linear interpolation between two colors.
///
/// When either input is `Color::Indexed`, the result is quantized back to
/// the nearest indexed color so the output stays terminal-compatible.
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let (ar, ag, ab) = color_to_rgb(a);
    let (br, bg, bb) = color_to_rgb(b);
    let t = t.clamp(0.0, 1.0);
    let r = (ar as f32 + (br as f32 - ar as f32) * t).round() as u8;
    let g = (ag as f32 + (bg as f32 - ag as f32) * t).round() as u8;
    let b_ch = (ab as f32 + (bb as f32 - ab as f32) * t).round() as u8;
    match (a, b) {
        (Color::Indexed(_), _) | (_, Color::Indexed(_)) => {
            Color::Indexed(crate::render::color::nearest_indexed(r, g, b_ch))
        }
        _ => Color::Rgb(r, g, b_ch),
    }
}

/// RGB for any color variant, using a neutral fallback for `Reset`.
///
/// Necessary so a gradient that lerps across named breakpoints (after
/// the theme has quantized to ANSI on lower-color terminals) still
/// produces meaningful intermediate colors instead of collapsing all
/// inputs onto one fallback.
fn color_to_rgb(c: Color) -> (u8, u8, u8) {
    // (198, 198, 198) matches the FG-equivalent used elsewhere when the
    // terminal owns the actual default fg color.
    crate::render::color::resolve_to_rgb(c).unwrap_or((198, 198, 198))
}

// ---------------------------------------------------------------------------
// Status bar separator
// ---------------------------------------------------------------------------

/// The separator character between status bar items.
pub const SEPARATOR: &str = "│";

// ---------------------------------------------------------------------------
// Context bar line builder
// ---------------------------------------------------------------------------

/// Width of the percentage field on hover (`fmt_pct5` always returns 5 chars).
const PCT_WIDTH: u16 = 5;
/// Width of the gap between the progress bar and the percentage on hover.
const BAR_PCT_GAP: u16 = 1;

// BAR_BG removed — use theme.bg_highlight directly (already quantized).

/// Build the context usage bar as a `Line<'static>`.
///
/// Normal: `8.5K / 1.0M` — actual token usage, colored by the same percentage
/// gradient the hover bar uses so the urgency signal stays visible at a glance.
/// Hovered: `█████ 42.0%` — progress bar + colored percentage, sized to match.
///
/// The bar width is derived from the default token string length so the
/// hovered line has the same total width as the default (no layout shift on
/// hover). The default is right-padded to a minimum of 6 columns
/// (`BAR_PCT_GAP + PCT_WIDTH`) so the invariant holds for every input — without
/// the pad, degenerate cases like `0 / 9` (5 chars) would mismatch the hovered
/// line, which always rounds up to 6 (zero-width bar + gap + percentage).
///
/// Returns `None` if token data is unavailable.
///
/// Gateway light-frontend (`kind: "chat"`) sessions must not display Build /
/// local sampler context usage — call with `gateway_chat = true` to suppress
/// the bar entirely (remote owns context; no mapped totals yet). remote settings
/// opt-in for chat entry can reuse the same gate later.
pub fn context_bar_line(
    used_tokens: Option<u64>,
    total_tokens: Option<u64>,
    hovered: bool,
    theme: &Theme,
) -> Option<Line<'static>> {
    context_bar_line_for_session(used_tokens, total_tokens, hovered, theme, false)
}

/// Like [`context_bar_line`], but omits the bar for gateway/chat-kind sessions.
pub fn context_bar_line_for_session(
    used_tokens: Option<u64>,
    total_tokens: Option<u64>,
    hovered: bool,
    theme: &Theme,
    gateway_chat: bool,
) -> Option<Line<'static>> {
    if gateway_chat {
        return None;
    }
    let used = used_tokens?;
    let total = total_tokens.filter(|&t| t > 0)?;
    let pct = echo_agent_token_estimation::usage_percentage(used, total);

    // Default form drives the line width: `used / total`, right-padded to the
    // minimum hover width so the two states always render at the same width.
    let mut token_str = format!("{} / {}", fmt_tokens(used), fmt_tokens(total));
    let natural_width = token_str.chars().count() as u16;
    let min_width = BAR_PCT_GAP + PCT_WIDTH;
    if natural_width < min_width {
        token_str.push_str(&" ".repeat((min_width - natural_width) as usize));
    }
    let total_width = natural_width.max(min_width);

    // Urgency color shared by both branches so the default still surfaces
    // high-usage warnings without requiring the user to hover.
    let breakpoints = default_breakpoints(theme);
    let color = crate::theme::quantize(blend_color(pct, &breakpoints));

    if hovered {
        // Bar fills the space the default tokens would occupy, minus the gap
        // and the percentage. `total_width >= min_width` by construction, so
        // this subtraction is safe.
        let bar_width = total_width - min_width;
        let mut spans =
            progress_bar_spans(bar_width, pct as f32 / 100.0, color, theme.bg_highlight);
        spans.push(Span::styled(" ", Style::default().bg(theme.bg_base)));
        spans.push(Span::styled(
            fmt_pct5(pct),
            Style::default().fg(theme.text_secondary).bg(theme.bg_base),
        ));
        Some(Line::from(spans))
    } else {
        Some(Line::from(Span::styled(
            token_str,
            Style::default().fg(color).bg(theme.bg_base),
        )))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fmt_pct5_under_10() {
        assert_eq!(fmt_pct5(0.0), "0.00%");
        assert_eq!(fmt_pct5(5.123), "5.12%");
        assert_eq!(fmt_pct5(9.99), "9.99%");
    }

    #[test]
    fn test_fmt_pct5_10_to_99() {
        assert_eq!(fmt_pct5(10.0), "10.0%");
        assert_eq!(fmt_pct5(20.16), "20.2%"); // rounds
        assert_eq!(fmt_pct5(99.9), "99.9%");
    }

    #[test]
    fn test_fmt_pct5_max() {
        assert_eq!(fmt_pct5(100.0), "MAX %");
        assert_eq!(fmt_pct5(150.0), "MAX %");
    }

    #[test]
    fn test_fmt_pct5_all_5_chars() {
        for pct in [0.0, 0.01, 1.0, 5.55, 9.99, 10.0, 50.0, 99.9, 100.0] {
            let s = fmt_pct5(pct);
            assert_eq!(s.len(), 5, "fmt_pct5({pct}) = {s:?} should be 5 chars");
        }
    }

    #[test]
    fn test_fmt_tokens_small() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(12), "12");
        assert_eq!(fmt_tokens(999), "999");
    }

    #[test]
    fn test_fmt_tokens_thousands() {
        assert_eq!(fmt_tokens(1_200), "1.2K");
        assert_eq!(fmt_tokens(9_960), "10.0K"); // rounds up
        assert_eq!(fmt_tokens(9_940), "9.9K");
        assert_eq!(fmt_tokens(12_000), "12K");
        assert_eq!(fmt_tokens(123_000), "123K");
        assert_eq!(fmt_tokens(999_000), "999K");
    }

    #[test]
    fn test_fmt_tokens_millions() {
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
        assert_eq!(fmt_tokens(12_000_000), "12M");
        assert_eq!(fmt_tokens(123_000_000), "123M");
    }

    #[test]
    fn test_fmt_tokens_max_4_chars() {
        for n in [
            0, 1, 999, 1_200, 9_900, 12_000, 999_000, 1_200_000, 12_000_000,
        ] {
            let s = fmt_tokens(n);
            assert!(s.len() <= 4, "fmt_tokens({n}) = {s:?} should be ≤4 chars");
        }
    }

    #[test]
    fn test_blend_color_at_breakpoints() {
        // Use unquantized theme — blend_color needs raw RGB values for lerp math.
        let theme = Theme::default();
        let bps = default_breakpoints(&theme);
        // At 0%, should be theme.text_primary
        let c0 = blend_color(0.0, &bps);
        assert_eq!(c0, theme.text_primary);
        // At 95%, should be theme.accent_error
        let c95 = blend_color(95.0, &bps);
        assert_eq!(c95, theme.accent_error);
    }

    /// Concatenate all span content into one string for assertions.
    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn test_context_bar_default_shows_compact_token_usage() {
        // Default (non-hovered) state shows `used / total` with no padding.
        let theme = Theme::default();
        let line = context_bar_line(Some(8_500), Some(1_000_000), false, &theme)
            .expect("token data provided");
        let text = line_text(&line);
        assert_eq!(text, "8.5K / 1.0M");
    }

    #[test]
    fn test_context_bar_hover_shows_bar_and_percentage() {
        // Hovered state shows the progress bar followed by the percentage.
        let theme = Theme::default();
        let line =
            context_bar_line(Some(420_000), Some(1_000_000), true, &theme).expect("token data");
        let text = line_text(&line);
        assert!(
            text.ends_with("42.0%"),
            "expected hovered line to end with '42.0%', got: {text:?}"
        );
    }

    #[test]
    fn test_context_bar_hover_width_matches_default() {
        // For each (used, total) combo, the hovered line must be the same
        // width as the default — toggling hover should never shift layout.
        let theme = Theme::default();
        for (used, total) in [
            (8_500u64, 1_000_000u64),
            (500, 1_000_000),
            (123_456, 1_000_000),
            (999_999, 999_999),
            (12_000_000, 12_000_000),
            // Degenerate sub-min-width case: default natural width is 5
            // ("0 / 9"), padded to 6 so the hover line still matches.
            (0, 9),
        ] {
            let default_line = context_bar_line(Some(used), Some(total), false, &theme)
                .expect("token data provided");
            let hover_line = context_bar_line(Some(used), Some(total), true, &theme)
                .expect("token data provided");
            assert_eq!(
                default_line.width(),
                hover_line.width(),
                "default vs hover width mismatch for used={used} total={total}: \
                 default={:?} hover={:?}",
                line_text(&default_line),
                line_text(&hover_line),
            );
        }
    }

    #[test]
    fn test_context_bar_hover_bar_grows_with_token_string() {
        // The bar size should scale with the default string length.
        // `500 / 1.0M` (10 chars) → bar = 10 - 6 = 4 chars.
        // `8.5K / 1.0M` (11 chars) → bar = 11 - 6 = 5 chars.
        let theme = Theme::default();
        let short = context_bar_line(Some(500), Some(1_000_000), true, &theme).unwrap();
        let long = context_bar_line(Some(8_500), Some(1_000_000), true, &theme).unwrap();
        assert!(
            short.width() < long.width(),
            "expected shorter default to produce shorter hover line; \
             short={:?} ({} cols), long={:?} ({} cols)",
            line_text(&short),
            short.width(),
            line_text(&long),
            long.width(),
        );
    }

    #[test]
    fn test_context_bar_returns_none_without_tokens() {
        // Mirror across hover states so a future refactor that moves the
        // unavailability checks into per-branch arms can't silently regress
        // one path.
        let theme = Theme::default();
        for hovered in [false, true] {
            assert!(context_bar_line(None, Some(1_000_000), hovered, &theme).is_none());
            assert!(context_bar_line(Some(1_000), None, hovered, &theme).is_none());
            // Zero total is treated as missing.
            assert!(context_bar_line(Some(1_000), Some(0), hovered, &theme).is_none());
        }
    }

    #[test]
    fn gateway_chat_suppresses_context_bar_even_with_tokens() {
        let theme = Theme::default();
        assert!(
            context_bar_line_for_session(Some(1_000), Some(1_000_000), false, &theme, true)
                .is_none()
        );
        assert!(
            context_bar_line_for_session(Some(1_000), Some(1_000_000), false, &theme, false)
                .is_some()
        );
    }
}
