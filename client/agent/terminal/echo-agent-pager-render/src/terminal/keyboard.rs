//! Per-terminal keyboard input capabilities.
//!
//! Classifies keyboard delivery semantics so input-handling code can consume one struct
//! instead of branching on brand. The classification depends on the
//! host OS — today only macOS rows are populated. Extend
//! [`KeyboardCapabilities`] with new fields (paste
//! protocol, focus reporting, custom escapes) instead of adding more
//! `match self.brand` sites scattered through the pager.

use super::TerminalName;
use crate::host::HostOs;
/// What happens to a single modifier (Cmd, Opt, etc.) on its way from
/// the keyboard to the program.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, strum::Display)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ModifierFate {
    /// Terminal delivers the modifier in the `KeyEvent` (KKP) or as a
    /// readline-equivalent byte sequence the textarea already handles
    /// (`^U`, `ESC ^?`).
    Native,
    /// Terminal drops the modifier; the OS-level rescue can recover it
    /// (CoreGraphics on macOS).
    Dropped,
    /// Chord captured before reaching the PTY (Apple Terminal Cmd+Bsp).
    /// No event arrives; not even an OS rescue helps.
    Unrecoverable,
    /// Behavior unclassified — treated as no-rescue to avoid false
    /// positives on unknown brands.
    #[default]
    Unknown,
}

impl ModifierFate {
    pub fn benefits_from_rescue(self) -> bool {
        matches!(self, Self::Dropped)
    }
}

/// How a terminal delivers Cmd/Opt-modified Backspace/Delete.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ModifierDelivery {
    pub cmd: ModifierFate,
    pub opt: ModifierFate,
}

impl ModifierDelivery {
    /// Construct a delivery from explicit fates. `#[non_exhaustive]` blocks
    /// struct-literal construction from other crates, so downstream test
    /// builds use this constructor.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_for_test(cmd: ModifierFate, opt: ModifierFate) -> Self {
        Self { cmd, opt }
    }

    pub fn benefits_from_rescue(self) -> bool {
        self.cmd.benefits_from_rescue() || self.opt.benefits_from_rescue()
    }

    pub fn label(self) -> String {
        format!("cmd={}, opt={}", self.cmd, self.opt)
    }
}

/// Per-terminal keyboard capabilities. Extend with new fields as more
/// per-terminal input behaviors get classified.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct KeyboardCapabilities {
    pub modifier_delivery: ModifierDelivery,
    /// Fate of Shift/Opt/Cmd when modifying `Enter`. Apple Terminal
    /// drops these and we recover them via the same OS poll used for
    /// Backspace/Delete.
    pub enter_modifier: ModifierFate,
}

impl KeyboardCapabilities {
    pub fn enter_needs_rescue(&self) -> bool {
        matches!(self.enter_modifier, ModifierFate::Dropped)
    }
}

/// Classify keyboard capabilities for the current host.
///
/// Today the table is populated only for macOS; other OSes return the
/// default (all-`Unknown`). When a Linux/Windows probe lands, add a
/// per-OS arm here rather than forking the function.
pub fn keyboard_capabilities(brand: TerminalName) -> KeyboardCapabilities {
    keyboard_capabilities_for_host(brand, HostOs::current())
}

/// Classify keyboard capabilities for explicit host evidence.
pub fn keyboard_capabilities_for_host(brand: TerminalName, host: HostOs) -> KeyboardCapabilities {
    match host {
        HostOs::Macos => macos_capabilities(brand),
        HostOs::Linux | HostOs::Windows | HostOs::Other => KeyboardCapabilities::default(),
    }
}

fn macos_capabilities(brand: TerminalName) -> KeyboardCapabilities {
    use ModifierFate::*;
    let (cmd, opt, enter) = match brand {
        TerminalName::Ghostty | TerminalName::Kitty | TerminalName::Foot => {
            (Native, Native, Native)
        }
        // iTerm2/VS Code translate Cmd+Bsp → ^U and Opt+Bsp → ESC ^?,
        // both of which the textarea already handles natively. VS Code-family
        // embeds and Zed inherit the same keymap behavior as VS Code
        // capabilities at runtime (no TERM_FEATURES, XTVERSION leaks).
        // (including the Cmd+Bsp → ^U translation).
        TerminalName::Iterm2
        | TerminalName::VsCode
        | TerminalName::Cursor
        | TerminalName::Windsurf
        | TerminalName::Zed => (Native, Native, Native),
        TerminalName::WezTerm => (Dropped, Native, Native),
        // Alacritty's macOS keymap binds Cmd+Bsp → ^U (native readline);
        // Opt+Bsp is a bare ^? without `option_as_alt` set.
        TerminalName::Alacritty | TerminalName::Rio => (Native, Dropped, Native),
        TerminalName::WarpTerminal => (Dropped, Dropped, Native),
        // Apple Terminal: Cmd+Bsp captured by the window manager.
        // Opt+Bsp and modified Enter are dropped; CG can rescue both.
        TerminalName::AppleTerminal => (Unrecoverable, Dropped, Dropped),
        TerminalName::EchoAgentDesktop => (Unknown, Unknown, Unknown),
        // VTE-based terminals (incl. Terminator) on macOS are unusual;
        // classify when we have evidence rather than guessing.
        TerminalName::Vte | TerminalName::Terminator => (Unknown, Unknown, Unknown),
        // JetBrains JediTerm: no KKP, no CG rescue. No way to probe
        // Mouse reporting has known SGR bugs in Classic engine (IJPL-232482);
        // Mouse reporting has known SGR bugs in Classic engine (IJPL-232482);
        // Reworked 2025 engine is better but indistinguishable via env vars.
        TerminalName::JetBrains => (Unknown, Unknown, Unknown),
        TerminalName::WindowsTerminal | TerminalName::Otty | TerminalName::Unknown => {
            (Unknown, Unknown, Unknown)
        }
    };
    KeyboardCapabilities {
        modifier_delivery: ModifierDelivery { cmd, opt },
        enter_modifier: enter,
    }
}

// Tests verify the macOS classification table; they only mean
// something on a macOS host. Cross-OS verification will need a test
// harness that overrides `HostOs::current()`; see the module doc.
#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn ghostty_and_kitty_native_no_rescue() {
        for brand in [TerminalName::Ghostty, TerminalName::Kitty] {
            let c = keyboard_capabilities(brand);
            assert_eq!(c.modifier_delivery.cmd, ModifierFate::Native);
            assert_eq!(c.modifier_delivery.opt, ModifierFate::Native);
            assert!(!c.modifier_delivery.benefits_from_rescue());
        }
    }

    #[test]
    fn iterm_and_vscode_readline_no_rescue() {
        for brand in [TerminalName::Iterm2, TerminalName::VsCode] {
            assert!(
                !keyboard_capabilities(brand)
                    .modifier_delivery
                    .benefits_from_rescue()
            );
        }
    }

    #[test]
    fn wezterm_drops_cmd_keeps_opt() {
        let c = keyboard_capabilities(TerminalName::WezTerm);
        assert_eq!(c.modifier_delivery.cmd, ModifierFate::Dropped);
        assert_eq!(c.modifier_delivery.opt, ModifierFate::Native);
        assert!(c.modifier_delivery.benefits_from_rescue());
    }

    #[test]
    fn alacritty_keeps_cmd_drops_opt() {
        let c = keyboard_capabilities(TerminalName::Alacritty);
        assert_eq!(c.modifier_delivery.cmd, ModifierFate::Native);
        assert_eq!(c.modifier_delivery.opt, ModifierFate::Dropped);
        assert!(c.modifier_delivery.benefits_from_rescue());
    }

    #[test]
    fn rio_keeps_cmd_drops_opt() {
        let c = keyboard_capabilities(TerminalName::Rio);
        assert_eq!(c.modifier_delivery.cmd, ModifierFate::Native);
        assert_eq!(c.modifier_delivery.opt, ModifierFate::Dropped);
        assert!(c.modifier_delivery.benefits_from_rescue());
    }

    #[test]
    fn apple_terminal_cmd_unrecoverable_opt_dropped() {
        let c = keyboard_capabilities(TerminalName::AppleTerminal);
        assert_eq!(c.modifier_delivery.cmd, ModifierFate::Unrecoverable);
        assert_eq!(c.modifier_delivery.opt, ModifierFate::Dropped);
        assert!(c.modifier_delivery.benefits_from_rescue());
        assert!(c.enter_needs_rescue());
    }

    #[test]
    fn unknown_brands_skip_rescue() {
        for brand in [
            TerminalName::Unknown,
            TerminalName::EchoAgentDesktop,
            TerminalName::Vte,
            TerminalName::JetBrains,
        ] {
            assert!(
                !keyboard_capabilities(brand)
                    .modifier_delivery
                    .benefits_from_rescue()
            );
        }
    }
}
