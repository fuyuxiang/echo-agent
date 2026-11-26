//! Terminal color support detection and quantization.
//!
//! Detects the terminal's color capabilities (truecolor / 256 / 16 / none) and
//! provides a [`quantize_color`] function that downgrades a [`ratatui::style::Color`]
//! to the highest level the terminal supports.
//!
//! The detected level is cached in a global [`OnceLock`] — call [`detect`] once
//! at startup, then use [`get`] everywhere else.

use std::sync::OnceLock;

use ratatui::style::Color;

use crate::render::color::{indexed_to_rgb, nearest_indexed};
use crate::terminal::{TerminalName, terminal_context};

/// Terminal color support level (ordered low → high).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorLevel {
    /// No color support (monochrome).
    None,
    /// Basic 16-color ANSI (SGR 30–37 / 90–97).
    Basic,
    /// 256-color indexed palette (SGR 38;5;N).
    Ansi256,
    /// 24-bit truecolor RGB (SGR 38;2;R;G;B).
    TrueColor,
}

impl ColorLevel {
    pub fn has_color(self) -> bool {
        self >= Self::Basic
    }

    pub fn has_256(self) -> bool {
        self >= Self::Ansi256
    }

    pub fn has_truecolor(self) -> bool {
        self >= Self::TrueColor
    }

    /// Canonical lowercase spelling that round-trips through the
    /// `ECHO_AGENT_FORCE_COLOR_LEVEL` parser. Use this in user-facing
    /// diagnostics (not `{:?}` Debug, which yields `Basic` / `Ansi256`
    /// / `TrueColor` / `None`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Basic => "basic",
            Self::Ansi256 => "256",
            Self::TrueColor => "truecolor",
        }
    }
}

impl std::fmt::Display for ColorLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Global singleton ─────────────────────────────────────────────────────

static COLOR_LEVEL: OnceLock<ColorLevel> = OnceLock::new();

/// Detect the terminal's color support and cache the result.
///
/// Uses the `supports-color` crate which checks `COLORTERM`, `TERM`,
/// terminal-specific env vars (`ITERM_SESSION_ID`, etc.) and whether
/// stdout is a TTY.
///
/// If `NO_COLOR` is set the result is [`ColorLevel::None`].
/// If stdout is not a TTY (test runner, piped output) and `NO_COLOR` is
/// absent, defaults to [`ColorLevel::TrueColor`] — the safe assumption
/// for a TUI app that always runs inside a terminal.
///
/// Capped at [`ColorLevel::Basic`] while the terminal-native lock is
/// engaged.
pub fn detect() -> ColorLevel {
    let raw = detect_raw();
    if crate::theme::cache::terminal_native_locked() {
        return raw.min(ColorLevel::Basic);
    }
    raw
}

/// The raw cached detection, without the terminal-native lock cap.
fn detect_raw() -> ColorLevel {
    *COLOR_LEVEL.get_or_init(|| {
        // Explicit opt-out via NO_COLOR takes priority.
        if std::env::var_os("NO_COLOR").is_some() {
            return ColorLevel::None;
        }

        let level = match supports_color::on(supports_color::Stream::Stdout) {
            Some(level) => {
                if level.has_16m {
                    ColorLevel::TrueColor
                } else if level.has_256 {
                    ColorLevel::Ansi256
                } else if level.has_basic {
                    ColorLevel::Basic
                } else {
                    ColorLevel::None
                }
            }
            // Not a TTY (tests, piped) — default to TrueColor.
            None => ColorLevel::TrueColor,
        };

        // The `supports-color` crate relies on COLORTERM=truecolor, but
        // tmux/SSH/mosh often strip that variable.  When the crate reports
        // only 256-color support, upgrade to TrueColor if we can identify
        // the terminal emulator and know it handles 24-bit RGB.
        if level < ColorLevel::TrueColor && terminal_supports_truecolor() {
            return ColorLevel::TrueColor;
        }

        level
    })
}

/// Standalone diagnostic color evidence.
///
/// This never consults stdout, because `echoagent doctor --json` is commonly piped.
/// Stderr or an independently opened controlling terminal is sufficient
/// evidence that the process is diagnosing that terminal; a fully headless
/// invocation is honest about having no color evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandaloneColorEvidence {
    Available(ColorLevel),
    Unavailable,
}

pub fn standalone(terminal: TerminalName) -> StandaloneColorEvidence {
    use std::io::IsTerminal;

    standalone_from_env(
        &crate::host::collect_unicode_env(),
        std::io::stderr().is_terminal(),
        controlling_terminal_available(),
        terminal,
    )
}

fn controlling_terminal_available() -> bool {
    #[cfg(unix)]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok()
    }
    #[cfg(windows)]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONIN$")
            .is_ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn standalone_from_env(
    env: &std::collections::HashMap<String, String>,
    stderr_is_terminal: bool,
    controlling_terminal: bool,
    terminal: TerminalName,
) -> StandaloneColorEvidence {
    if env.contains_key("NO_COLOR") {
        return StandaloneColorEvidence::Available(ColorLevel::None);
    }
    if !stderr_is_terminal && !controlling_terminal {
        return StandaloneColorEvidence::Unavailable;
    }
    let colorterm = env.get("COLORTERM").map(|value| value.to_ascii_lowercase());
    if colorterm
        .as_deref()
        .is_some_and(|value| value == "truecolor" || value == "24bit")
        || terminal_supports_truecolor_brand(terminal)
    {
        return StandaloneColorEvidence::Available(ColorLevel::TrueColor);
    }
    let term = env
        .get("TERM")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if term.contains("256color") {
        StandaloneColorEvidence::Available(ColorLevel::Ansi256)
    } else if term.is_empty() || term == "dumb" {
        StandaloneColorEvidence::Unavailable
    } else {
        StandaloneColorEvidence::Available(ColorLevel::Basic)
    }
}

/// Return the cached color level (calls [`detect`] if not yet initialized).
pub fn get() -> ColorLevel {
    detect()
}

/// Override the color level (useful for tests or `--color` flags).
///
/// Returns `Err` if already set.
pub fn set(level: ColorLevel) -> Result<(), ColorLevel> {
    COLOR_LEVEL.set(level)
}

// ── Color quantization ──────────────────────────────────────────────────

/// Downgrade a [`Color`] to the highest representation the terminal supports.
///
/// | Terminal level | `Rgb`            | `Indexed`         | Named (`Red`…) |
/// |----------------|------------------|--------------------|----------------|
/// | TrueColor      | pass-through     | pass-through       | pass-through   |
/// | Ansi256        | → nearest idx    | pass-through       | pass-through   |
/// | Basic          | → nearest ANSI16 | → nearest ANSI16   | pass-through   |
/// | None           | → `Reset`        | → `Reset`          | → `Reset`      |
pub fn quantize_color(color: Color, level: ColorLevel) -> Color {
    match level {
        ColorLevel::TrueColor => color,
        ColorLevel::Ansi256 => match color {
            Color::Rgb(r, g, b) => Color::Indexed(nearest_indexed(r, g, b)),
            other => other,
        },
        ColorLevel::Basic => match color {
            Color::Rgb(r, g, b) => indexed_to_ansi16(nearest_indexed(r, g, b)),
            Color::Indexed(n) => indexed_to_ansi16(n),
            other => other,
        },
        ColorLevel::None => Color::Reset,
    }
}

/// Quantize a color using the globally-detected level.
pub fn quantize(color: Color) -> Color {
    quantize_color(color, get())
}

// ── Terminal-based truecolor inference ──────────────────────────────────

/// Check whether the detected terminal emulator is known to support truecolor.
///
/// Used as a fallback when `COLORTERM` is missing (e.g. inside tmux, SSH, or
/// — most importantly — under a bare `cmd.exe` / `powershell.exe` ConHost
/// window, which has supported VT-encoded 24-bit color since Windows 10
/// 1709 (Fall Creators Update) but doesn't advertise it via COLORTERM. Without
/// this fallback our themes get quantized to the 16-color ANSI palette there
/// and the subtle bg/border/muted gradations collapse onto each other.
fn terminal_supports_truecolor() -> bool {
    terminal_supports_truecolor_brand(terminal_context().brand)
}

fn terminal_supports_truecolor_brand(terminal: TerminalName) -> bool {
    if matches!(
        terminal,
        TerminalName::Iterm2
            | TerminalName::Ghostty
            | TerminalName::Kitty
            | TerminalName::WezTerm
            | TerminalName::Alacritty
            | TerminalName::Rio
            | TerminalName::WarpTerminal
            | TerminalName::VsCode
            | TerminalName::WindowsTerminal
            | TerminalName::Foot
    ) {
        return true;
    }
    // Native Windows: assume ConHost has VT processing enabled. Pre-1709
    // hosts are effectively extinct and would gracefully degrade by
    // ignoring the SGR 38;2;... sequences.
    cfg!(target_os = "windows")
}

// ── 256 → 16 mapping ────────────────────────────────────────────────────

/// Map a 256-color index to the nearest basic ANSI 16 color.
fn indexed_to_ansi16(n: u8) -> Color {
    match n {
        // First 16 indices already *are* the ANSI 16 colors.
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::White, // actually "silver" in most terminals
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        15 => Color::White,
        // For 16–255, convert to RGB and find nearest ANSI 16 color.
        _ => {
            let (r, g, b) = indexed_to_rgb(n);
            rgb_to_ansi16(r, g, b)
        }
    }
}

/// Find the nearest ANSI 16 color for an RGB triplet.
///
/// Uses a simple squared-Euclidean distance over the standard xterm ANSI 16
/// palette. Good enough for a fallback — 16-color terminals are very rare.
fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> Color {
    // Standard xterm ANSI 16 palette (same values used by indexed_to_rgb for 0–15).
    const PALETTE: [(u8, u8, u8, Color); 16] = [
        (0, 0, 0, Color::Black),
        (128, 0, 0, Color::Red),
        (0, 128, 0, Color::Green),
        (128, 128, 0, Color::Yellow),
        (0, 0, 128, Color::Blue),
        (128, 0, 128, Color::Magenta),
        (0, 128, 128, Color::Cyan),
        (192, 192, 192, Color::White),
        (128, 128, 128, Color::DarkGray),
        (255, 0, 0, Color::LightRed),
        (0, 255, 0, Color::LightGreen),
        (255, 255, 0, Color::LightYellow),
        (0, 0, 255, Color::LightBlue),
        (255, 0, 255, Color::LightMagenta),
        (0, 255, 255, Color::LightCyan),
        (255, 255, 255, Color::White), // index 15 = bright white
    ];

    let mut best = Color::White;
    let mut best_dist = u32::MAX;
    for &(pr, pg, pb, color) in &PALETTE {
        let dr = r as i32 - pr as i32;
        let dg = g as i32 - pg as i32;
        let db = b as i32 - pb as i32;
        let dist = (dr * dr + dg * dg + db * db) as u32;
        if dist < best_dist {
            best_dist = dist;
            best = color;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn standalone_color_uses_stderr_terminal_not_stdout() {
        assert_eq!(
            standalone_from_env(
                &env(&[("COLORTERM", "truecolor")]),
                true,
                false,
                TerminalName::Unknown,
            ),
            StandaloneColorEvidence::Available(ColorLevel::TrueColor)
        );
        assert_eq!(
            standalone_from_env(
                &env(&[("TERM", "xterm-256color")]),
                true,
                false,
                TerminalName::Unknown,
            ),
            StandaloneColorEvidence::Available(ColorLevel::Ansi256)
        );
        assert_eq!(
            standalone_from_env(
                &env(&[("COLORTERM", "truecolor")]),
                false,
                false,
                TerminalName::Ghostty,
            ),
            StandaloneColorEvidence::Unavailable
        );
        assert_eq!(
            standalone_from_env(
                &env(&[("NO_COLOR", "1")]),
                false,
                false,
                TerminalName::Ghostty,
            ),
            StandaloneColorEvidence::Available(ColorLevel::None)
        );
    }

    #[test]
    fn standalone_color_uses_controlling_terminal_when_stderr_is_piped() {
        assert_eq!(
            standalone_from_env(
                &env(&[("TERM", "xterm-256color")]),
                false,
                true,
                TerminalName::Unknown,
            ),
            StandaloneColorEvidence::Available(ColorLevel::Ansi256)
        );
    }

    #[test]
    fn standalone_color_uses_known_terminal_brand_without_colorterm() {
        assert_eq!(
            standalone_from_env(
                &env(&[("TERM", "xterm")]),
                true,
                false,
                TerminalName::WezTerm,
            ),
            StandaloneColorEvidence::Available(ColorLevel::TrueColor)
        );
    }

    #[test]
    fn truecolor_passes_through() {
        let rgb = Color::Rgb(122, 162, 247);
        assert_eq!(quantize_color(rgb, ColorLevel::TrueColor), rgb);

        let idx = Color::Indexed(141);
        assert_eq!(quantize_color(idx, ColorLevel::TrueColor), idx);
    }

    #[test]
    fn ansi256_quantizes_rgb_to_indexed() {
        let rgb = Color::Rgb(122, 162, 247);
        let q = quantize_color(rgb, ColorLevel::Ansi256);
        assert!(matches!(q, Color::Indexed(_)));
    }

    #[test]
    fn ansi256_passes_indexed_through() {
        let idx = Color::Indexed(141);
        assert_eq!(quantize_color(idx, ColorLevel::Ansi256), idx);
    }

    #[test]
    fn basic_quantizes_to_named() {
        let rgb = Color::Rgb(255, 0, 0);
        let q = quantize_color(rgb, ColorLevel::Basic);
        // Should map to a red variant
        assert!(
            matches!(q, Color::Red | Color::LightRed),
            "expected Red/LightRed, got {q:?}"
        );
    }

    #[test]
    fn basic_quantizes_indexed_to_named() {
        // Indexed(196) = (255,0,0) — pure bright red in the cube
        let idx = Color::Indexed(196);
        let q = quantize_color(idx, ColorLevel::Basic);
        assert!(
            matches!(q, Color::Red | Color::LightRed),
            "expected Red/LightRed, got {q:?}"
        );
    }

    #[test]
    fn none_resets_everything() {
        assert_eq!(
            quantize_color(Color::Rgb(100, 200, 50), ColorLevel::None),
            Color::Reset
        );
        assert_eq!(
            quantize_color(Color::Indexed(111), ColorLevel::None),
            Color::Reset
        );
    }

    #[test]
    fn named_colors_pass_through_all_levels() {
        for level in [
            ColorLevel::TrueColor,
            ColorLevel::Ansi256,
            ColorLevel::Basic,
        ] {
            assert_eq!(quantize_color(Color::Red, level), Color::Red);
            assert_eq!(quantize_color(Color::Blue, level), Color::Blue);
        }
    }

    #[test]
    fn level_ordering() {
        assert!(ColorLevel::None < ColorLevel::Basic);
        assert!(ColorLevel::Basic < ColorLevel::Ansi256);
        assert!(ColorLevel::Ansi256 < ColorLevel::TrueColor);
    }

    #[test]
    fn ansi16_roundtrip_first_16() {
        // Indices 0–15 should map to their corresponding named colors
        assert_eq!(indexed_to_ansi16(0), Color::Black);
        assert_eq!(indexed_to_ansi16(1), Color::Red);
        assert_eq!(indexed_to_ansi16(4), Color::Blue);
        assert_eq!(indexed_to_ansi16(9), Color::LightRed);
        assert_eq!(indexed_to_ansi16(14), Color::LightCyan);
    }
}
