//! Per-terminal hyperlink (OSC 8) capabilities.
//!
//! Classifies caller semantics so input-handling code can consume one struct
//! instead of branching on brand.

use super::TerminalName;

/// Whether the terminal supports OSC 8 hyperlink sequences.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, strum::Display)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum Osc8Support {
    /// Terminal natively supports OSC 8 sequences.
    Native,
    /// Terminal actively garbles unknown OSC sequences (Apple Terminal).
    HostileParser,
    /// Terminal explicitly does not support OSC 8.
    Unsupported,
    /// Support status is unknown.
    #[default]
    Unknown,
}

/// Which URL schemes the terminal supports in OSC 8 links.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, strum::Display)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum SchemeFilter {
    /// Standard web schemes: http, https, mailto.
    #[default]
    Standard,
    /// Extended editor schemes: vscode://, cursor://, idea://, zed://.
    EditorExtended,
}

impl SchemeFilter {
    /// Returns `true` if the given scheme is permitted by this filter.
    pub fn allows(&self, scheme: &str) -> bool {
        match self {
            Self::Standard => matches!(scheme, "http" | "https" | "mailto"),
            Self::EditorExtended => matches!(
                scheme,
                "http" | "https" | "mailto" | "file" | "vscode" | "cursor" | "idea" | "zed"
            ),
        }
    }
}

/// Per-terminal hyperlink capabilities.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct HyperlinkCapabilities {
    /// OSC 8 support level.
    pub osc8: Osc8Support,
    /// Whether the terminal supports the `id=` parameter for hover-grouping.
    pub id_param: bool,
    /// Which URL schemes the terminal handles.
    pub scheme_filter: SchemeFilter,
    /// Whether the terminal supports OSC 22 cursor-shape changes
    /// (e.g. switching to a hand/pointer cursor on link hover).
    pub osc22_cursor: bool,
    /// Whether the terminal handles link hover styling natively (so our
    /// app should skip its own Cmd/Ctrl+hover highlight logic).
    pub native_link_hover: bool,
    /// Terminal opens bare http(s)/mailto under mouse reporting (Warp).
    pub native_plain_url_open: bool,
}

/// Classify hyperlink capabilities for a given `brand`.
pub fn hyperlink_capabilities(brand: TerminalName) -> HyperlinkCapabilities {
    use Osc8Support::*;
    match brand {
        // Apple Terminal actively garbles unknown OSC sequences.
        TerminalName::AppleTerminal => HyperlinkCapabilities {
            osc8: HostileParser,
            id_param: false,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: false,
            native_link_hover: false,
            native_plain_url_open: false,
        },
        // Reference implementation. Excellent id= handling.
        TerminalName::Iterm2 => HyperlinkCapabilities {
            osc8: Native,
            id_param: true,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: true,
            native_link_hover: false,
            native_plain_url_open: false,
        },
        TerminalName::Ghostty => HyperlinkCapabilities {
            osc8: Native,
            id_param: true,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: true,
            native_link_hover: false,
            native_plain_url_open: false,
        },
        // Since kitty v0.19.
        TerminalName::Kitty => HyperlinkCapabilities {
            osc8: Native,
            id_param: true,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: true,
            native_link_hover: false,
            native_plain_url_open: false,
        },
        // Since Alacritty v0.11. Rio and foot also support OSC 8.
        TerminalName::Alacritty | TerminalName::Rio | TerminalName::Foot => HyperlinkCapabilities {
            osc8: Native,
            id_param: true,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: false,
            native_link_hover: false,
            native_plain_url_open: false,
        },
        TerminalName::WezTerm => HyperlinkCapabilities {
            osc8: Native,
            id_param: true,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: false,
            native_link_hover: false,
            native_plain_url_open: false,
        },
        // VS Code integrated terminal since v1.72. VS Code-family embeds
        // inherit the same terminal renderer (xterm.js). Zed implements
        // OSC 8 with similar capabilities. All of these handle link hover
        // styling natively.
        TerminalName::VsCode
        | TerminalName::Cursor
        | TerminalName::Windsurf
        | TerminalName::Zed => HyperlinkCapabilities {
            osc8: Native,
            id_param: true,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: false,
            native_link_hover: true,
            native_plain_url_open: false,
        },
        // Open issue warpdotdev/Warp#4194. UrlLocator opens bare URLs under
        // mouse reporting; keep native_link_hover false for file:// fallback.
        TerminalName::WarpTerminal => HyperlinkCapabilities {
            osc8: Unsupported,
            id_param: false,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: false,
            native_link_hover: false,
            native_plain_url_open: true,
        },
        // VTE-based terminals (GNOME Terminal, Terminator, etc.).
        // Conservative -- gated by version in the route resolver if
        // vte_version is too old.
        TerminalName::Vte | TerminalName::Terminator => HyperlinkCapabilities {
            osc8: Native,
            id_param: true,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: false,
            native_link_hover: false,
            native_plain_url_open: false,
        },
        // Windows Terminal since v1.4 (OSC 8 support).
        TerminalName::WindowsTerminal => HyperlinkCapabilities {
            osc8: Native,
            id_param: true,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: false,
            native_link_hover: false,
            native_plain_url_open: false,
        },
        // JetBrains JediTerm: OSC 8 varies across IDE versions; no
        // runtime probe available (no TERM_FEATURES). Conservative.
        TerminalName::JetBrains => HyperlinkCapabilities {
            osc8: Unknown,
            id_param: false,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: false,
            native_link_hover: false,
            native_plain_url_open: false,
        },
        // Electron app; behavior undocumented.
        TerminalName::EchoAgentDesktop => HyperlinkCapabilities {
            osc8: Unknown,
            id_param: false,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: false,
            native_link_hover: false,
            native_plain_url_open: false,
        },
        TerminalName::Otty | TerminalName::Unknown => HyperlinkCapabilities {
            osc8: Unknown,
            id_param: false,
            scheme_filter: SchemeFilter::Standard,
            osc22_cursor: false,
            native_link_hover: false,
            native_plain_url_open: false,
        },
    }
}

// ── OSC 22 cursor-shape commands ──────────────────────────────────────
//
// These wrap raw OSC 22 sequences as crossterm `Command`s so call sites
// can use `crossterm::execute!` / `queue!` instead of manual byte writes.

/// OSC 22: set the mouse pointer to the "pointer" (hand) shape.
///
/// Supported by iTerm2, Ghostty, and Kitty. Silently ignored by
/// terminals that don't understand OSC 22.
pub struct SetPointerCursor;

impl crossterm::Command for SetPointerCursor {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str("\x1b]22;pointer\x1b\\")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// OSC 22: reset the mouse pointer to the default (arrow) shape.
pub struct SetDefaultCursor;

impl crossterm::Command for SetDefaultCursor {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str("\x1b]22;default\x1b\\")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_terminal_hostile_parser() {
        let caps = hyperlink_capabilities(TerminalName::AppleTerminal);
        assert_eq!(caps.osc8, Osc8Support::HostileParser);
        assert!(!caps.id_param);
    }

    #[test]
    fn iterm2_native_with_id() {
        let caps = hyperlink_capabilities(TerminalName::Iterm2);
        assert_eq!(caps.osc8, Osc8Support::Native);
        assert!(caps.id_param);
    }

    #[test]
    fn warp_unsupported() {
        let caps = hyperlink_capabilities(TerminalName::WarpTerminal);
        assert_eq!(caps.osc8, Osc8Support::Unsupported);
        assert!(!caps.id_param);
        assert!(caps.native_plain_url_open);
        assert!(!caps.native_link_hover);
    }

    #[test]
    fn native_plain_url_open_only_warp() {
        assert!(hyperlink_capabilities(TerminalName::WarpTerminal).native_plain_url_open);
        for brand in [
            TerminalName::Iterm2,
            TerminalName::VsCode,
            TerminalName::AppleTerminal,
            TerminalName::Ghostty,
            TerminalName::Unknown,
        ] {
            assert!(
                !hyperlink_capabilities(brand).native_plain_url_open,
                "{brand:?} must not set native_plain_url_open"
            );
        }
    }

    #[test]
    fn unknown_terminal_unknown_support() {
        let caps = hyperlink_capabilities(TerminalName::Unknown);
        assert_eq!(caps.osc8, Osc8Support::Unknown);
        assert!(!caps.id_param);
    }

    #[test]
    fn scheme_filter_standard_allows_http() {
        assert!(SchemeFilter::Standard.allows("http"));
        assert!(SchemeFilter::Standard.allows("https"));
        assert!(SchemeFilter::Standard.allows("mailto"));
        assert!(!SchemeFilter::Standard.allows("file"));
        assert!(!SchemeFilter::Standard.allows("vscode"));
        assert!(!SchemeFilter::Standard.allows("javascript"));
    }

    #[test]
    fn scheme_filter_extended_allows_editor_schemes() {
        assert!(SchemeFilter::EditorExtended.allows("http"));
        assert!(SchemeFilter::EditorExtended.allows("vscode"));
        assert!(SchemeFilter::EditorExtended.allows("cursor"));
        assert!(SchemeFilter::EditorExtended.allows("idea"));
        assert!(SchemeFilter::EditorExtended.allows("zed"));
        assert!(!SchemeFilter::EditorExtended.allows("javascript"));
    }
}
