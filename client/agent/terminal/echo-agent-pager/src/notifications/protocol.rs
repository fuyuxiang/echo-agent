use std::borrow::Cow;
use std::io::Write;

use crate::notifications::tmux;
use crate::terminal::{MultiplexerKind, TerminalContext, TerminalName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationProtocol {
    /// iTerm2/WezTerm/Warp: `\x1b]9;{message}\x07`
    Osc9,
    /// Kitty: `\x1b]99;i=echoagent;{message}\x1b\\`
    Osc99,
    /// Ghostty/VTE: `\x1b]777;notify;{title};{body}\x1b\\`
    Osc777,
    /// Universal fallback: `\x07`
    Bel,
    /// No notification capability
    None,
}

impl NotificationProtocol {
    /// Stable lowercase name for telemetry and analytics output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Osc9 => "osc9",
            Self::Osc99 => "osc99",
            Self::Osc777 => "osc777",
            Self::Bel => "bel",
            Self::None => "none",
        }
    }
}

/// Choose the best notification protocol for the current terminal environment.
pub fn select_protocol(ctx: &TerminalContext) -> NotificationProtocol {
    if ctx.multiplexer == MultiplexerKind::Zellij {
        return NotificationProtocol::Bel;
    }
    match ctx.brand {
        TerminalName::Iterm2 | TerminalName::WezTerm | TerminalName::WarpTerminal => {
            NotificationProtocol::Osc9
        }
        TerminalName::Kitty => NotificationProtocol::Osc99,
        TerminalName::Ghostty
        | TerminalName::Vte
        | TerminalName::Terminator
        | TerminalName::Foot => NotificationProtocol::Osc777,
        TerminalName::EchoAgentDesktop => NotificationProtocol::None,
        TerminalName::AppleTerminal
        | TerminalName::Alacritty
        | TerminalName::Rio
        | TerminalName::VsCode
        | TerminalName::WindowsTerminal
        | TerminalName::JetBrains
        | TerminalName::Cursor
        | TerminalName::Windsurf
        | TerminalName::Zed
        | TerminalName::Otty
        | TerminalName::Unknown => NotificationProtocol::Bel,
    }
}

const BEL_BYTE: &[u8] = b"\x07";

/// Strip C0/C1 controls (including BEL and C1 ST) so model-derived titles
/// cannot terminate OSC/DCS early. Same filter as tab-title construction.
fn sanitize_osc_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Build the OSC/BEL payload. `None` means emit nothing.
fn notification_sequence(
    protocol: NotificationProtocol,
    title: &str,
    body: &str,
) -> Option<Cow<'static, str>> {
    let title = sanitize_osc_text(title);
    let body = sanitize_osc_text(body);
    Some(match protocol {
        // Body-only protocols fold the title (session name) into the body.
        // OSC 777 already uses the tab title as subtitle, so keep "EchoAgent".
        NotificationProtocol::Osc9 => format!("\x1b]9;{body} \u{b7} {title}\x07").into(),
        NotificationProtocol::Osc99 => {
            format!("\x1b]99;i=echoagent;{body} \u{b7} {title}\x1b\\").into()
        }
        NotificationProtocol::Osc777 => format!("\x1b]777;notify;EchoAgent;{body}\x1b\\").into(),
        NotificationProtocol::Bel => Cow::Borrowed("\x07"),
        NotificationProtocol::None => return None,
    })
}

/// Build the escape sequence for a notification, then write it to stderr.
///
/// When running under tmux the sequence is wrapped in DCS passthrough so the
/// outer terminal sees it.
pub fn emit_notification(
    protocol: NotificationProtocol,
    title: &str,
    body: &str,
    ctx: &TerminalContext,
) {
    let Some(sequence) = notification_sequence(protocol, title, body) else {
        return;
    };

    if ctx.is_tmux_backed() {
        let wrapped = tmux::tmux_passthrough(&sequence);
        echo_agent_runtime::util::with_locked_stderr(|stderr| {
            let _ = stderr.write_all(wrapped.as_bytes());
            let _ = stderr.flush();
        });
    } else if matches!(protocol, NotificationProtocol::Bel) {
        echo_agent_runtime::util::with_locked_stderr(|stderr| {
            let _ = stderr.write_all(BEL_BYTE);
            let _ = stderr.flush();
        });
    } else {
        let bytes = sequence.as_bytes();
        echo_agent_runtime::util::with_locked_stderr(|stderr| {
            let _ = stderr.write_all(bytes);
            let _ = stderr.flush();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{MultiplexerKind, TerminalContext, TerminalName};

    fn ctx_with_brand(brand: TerminalName) -> TerminalContext {
        TerminalContext {
            brand,
            ..Default::default()
        }
    }

    fn ctx_with_brand_and_mux(brand: TerminalName, mux: MultiplexerKind) -> TerminalContext {
        TerminalContext {
            brand,
            multiplexer: mux,
            ..Default::default()
        }
    }

    // --- select_protocol: every TerminalName variant ---

    #[test]
    fn select_iterm2_uses_osc9() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::Iterm2)),
            NotificationProtocol::Osc9
        );
    }

    #[test]
    fn select_wezterm_uses_osc9() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::WezTerm)),
            NotificationProtocol::Osc9
        );
    }

    #[test]
    fn select_warp_uses_osc9() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::WarpTerminal)),
            NotificationProtocol::Osc9
        );
    }

    #[test]
    fn select_kitty_uses_osc99() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::Kitty)),
            NotificationProtocol::Osc99
        );
    }

    #[test]
    fn select_ghostty_uses_osc777() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::Ghostty)),
            NotificationProtocol::Osc777
        );
    }

    #[test]
    fn select_vte_uses_osc777() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::Vte)),
            NotificationProtocol::Osc777
        );
    }

    #[test]
    fn select_echo_agent_desktop_uses_none() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::EchoAgentDesktop)),
            NotificationProtocol::None
        );
    }

    #[test]
    fn select_apple_terminal_uses_bel() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::AppleTerminal)),
            NotificationProtocol::Bel
        );
    }

    #[test]
    fn select_alacritty_uses_bel() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::Alacritty)),
            NotificationProtocol::Bel
        );
    }

    #[test]
    fn select_vscode_uses_bel() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::VsCode)),
            NotificationProtocol::Bel
        );
    }

    #[test]
    fn select_unknown_uses_bel() {
        assert_eq!(
            select_protocol(&ctx_with_brand(TerminalName::Unknown)),
            NotificationProtocol::Bel
        );
    }

    // --- Zellij override ---

    #[test]
    fn zellij_overrides_to_bel_for_kitty() {
        assert_eq!(
            select_protocol(&ctx_with_brand_and_mux(
                TerminalName::Kitty,
                MultiplexerKind::Zellij
            )),
            NotificationProtocol::Bel
        );
    }

    #[test]
    fn zellij_overrides_to_bel_for_ghostty() {
        assert_eq!(
            select_protocol(&ctx_with_brand_and_mux(
                TerminalName::Ghostty,
                MultiplexerKind::Zellij
            )),
            NotificationProtocol::Bel
        );
    }

    #[test]
    fn zellij_overrides_to_bel_for_iterm2() {
        assert_eq!(
            select_protocol(&ctx_with_brand_and_mux(
                TerminalName::Iterm2,
                MultiplexerKind::Zellij
            )),
            NotificationProtocol::Bel
        );
    }

    #[test]
    fn zellij_overrides_to_bel_for_wezterm() {
        assert_eq!(
            select_protocol(&ctx_with_brand_and_mux(
                TerminalName::WezTerm,
                MultiplexerKind::Zellij
            )),
            NotificationProtocol::Bel
        );
    }

    // --- tmux does NOT override protocol selection (passthrough is handled
    //     at emission time, not selection time) ---

    #[test]
    fn tmux_preserves_osc9_for_iterm2() {
        assert_eq!(
            select_protocol(&ctx_with_brand_and_mux(
                TerminalName::Iterm2,
                MultiplexerKind::Tmux
            )),
            NotificationProtocol::Osc9
        );
    }

    #[test]
    fn tmux_preserves_osc99_for_kitty() {
        assert_eq!(
            select_protocol(&ctx_with_brand_and_mux(
                TerminalName::Kitty,
                MultiplexerKind::Tmux
            )),
            NotificationProtocol::Osc99
        );
    }

    // --- screen does not override ---

    #[test]
    fn screen_preserves_osc777_for_ghostty() {
        assert_eq!(
            select_protocol(&ctx_with_brand_and_mux(
                TerminalName::Ghostty,
                MultiplexerKind::Screen
            )),
            NotificationProtocol::Osc777
        );
    }

    // --- emit_notification: verifies None is a no-op (does not panic) ---

    #[test]
    fn emit_none_is_noop() {
        let ctx = ctx_with_brand(TerminalName::EchoAgentDesktop);
        // Should return immediately without writing anything.
        emit_notification(NotificationProtocol::None, "title", "body", &ctx);
    }

    #[test]
    fn emit_bel_does_not_panic() {
        let ctx = ctx_with_brand(TerminalName::Unknown);
        emit_notification(NotificationProtocol::Bel, "", "", &ctx);
    }

    #[test]
    fn emit_osc9_does_not_panic() {
        let ctx = ctx_with_brand(TerminalName::Iterm2);
        emit_notification(NotificationProtocol::Osc9, "title", "body", &ctx);
    }

    #[test]
    fn emit_osc99_does_not_panic() {
        let ctx = ctx_with_brand(TerminalName::Kitty);
        emit_notification(NotificationProtocol::Osc99, "title", "body", &ctx);
    }

    #[test]
    fn emit_osc777_does_not_panic() {
        let ctx = ctx_with_brand(TerminalName::Ghostty);
        emit_notification(NotificationProtocol::Osc777, "title", "body", &ctx);
    }

    #[test]
    fn notification_sequence_strips_controls_from_title_and_body() {
        let seq = notification_sequence(
            NotificationProtocol::Osc9,
            "ti\x1btle\u{9c}",
            "bo\x07dy\x18",
        )
        .expect("osc9 yields a sequence");
        assert_eq!(seq.as_ref(), "\x1b]9;body \u{b7} title\x07");
    }

    #[test]
    fn notification_sequence_osc99_and_osc777_strip_controls() {
        let osc99 = notification_sequence(NotificationProtocol::Osc99, "t\x1b", "b\x07")
            .expect("osc99 yields a sequence");
        assert_eq!(osc99.as_ref(), "\x1b]99;i=echoagent;b \u{b7} t\x1b\\");

        let osc777 = notification_sequence(NotificationProtocol::Osc777, "ignored\x1b", "b\x1body")
            .expect("osc777 yields a sequence");
        assert_eq!(osc777.as_ref(), "\x1b]777;notify;EchoAgent;body\x1b\\");
    }

    #[test]
    fn notification_sequence_none_is_none() {
        assert!(notification_sequence(NotificationProtocol::None, "t", "b").is_none());
    }

    // --- exhaustive brand coverage in a table-driven test ---

    #[test]
    fn all_brands_have_defined_protocol() {
        let cases: &[(TerminalName, NotificationProtocol)] = &[
            (TerminalName::Iterm2, NotificationProtocol::Osc9),
            (TerminalName::WezTerm, NotificationProtocol::Osc9),
            (TerminalName::WarpTerminal, NotificationProtocol::Osc9),
            (TerminalName::Kitty, NotificationProtocol::Osc99),
            (TerminalName::Ghostty, NotificationProtocol::Osc777),
            (TerminalName::Vte, NotificationProtocol::Osc777),
            (TerminalName::Foot, NotificationProtocol::Osc777),
            (TerminalName::EchoAgentDesktop, NotificationProtocol::None),
            (TerminalName::AppleTerminal, NotificationProtocol::Bel),
            (TerminalName::Alacritty, NotificationProtocol::Bel),
            (TerminalName::VsCode, NotificationProtocol::Bel),
            (TerminalName::WindowsTerminal, NotificationProtocol::Bel),
            (TerminalName::Unknown, NotificationProtocol::Bel),
        ];

        for &(brand, expected) in cases {
            let ctx = ctx_with_brand(brand);
            assert_eq!(
                select_protocol(&ctx),
                expected,
                "protocol mismatch for {brand:?}"
            );
        }
    }

    #[test]
    fn zellij_forces_bel_for_all_osc_brands() {
        let osc_brands = [
            TerminalName::Iterm2,
            TerminalName::WezTerm,
            TerminalName::WarpTerminal,
            TerminalName::Kitty,
            TerminalName::Ghostty,
            TerminalName::Vte,
        ];
        for brand in osc_brands {
            let ctx = ctx_with_brand_and_mux(brand, MultiplexerKind::Zellij);
            assert_eq!(
                select_protocol(&ctx),
                NotificationProtocol::Bel,
                "zellij should force BEL for {brand:?}"
            );
        }
    }
}
