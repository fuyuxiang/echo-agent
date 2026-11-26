//! Per-environment hyperlink route policy.
//!
//! Mirrors [`crate::clipboard::resolve_clipboard_route`]. Combines per-brand
//! [`HyperlinkCapabilities`] with multiplexer/SSH/Byobu state into a single
//! decision struct cached once per process.

use std::sync::OnceLock;

use crate::terminal::{Osc8Support, TerminalContext};

/// Describes the hyperlink strategy for the current environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HyperlinkRoute {
    /// Whether to emit OSC 8 sequences around link text.
    pub emit_osc8: bool,
    /// Whether to include the `id=` parameter in OSC 8 sequences
    /// (enables hover-grouping across wrapped lines).
    pub emit_id: bool,
    /// Human-readable reason why OSC 8 is disabled, or `None` if enabled.
    pub skip_reason: Option<&'static str>,
}

/// Cached hyperlink route resolved at first use from the terminal context.
pub fn hyperlink_route() -> &'static HyperlinkRoute {
    static ROUTE: OnceLock<HyperlinkRoute> = OnceLock::new();
    ROUTE.get_or_init(|| {
        let ctx = crate::terminal::terminal_context();
        resolve_hyperlink_route(ctx)
    })
}

/// Resolve the hyperlink route from a terminal context.
pub fn resolve_hyperlink_route(ctx: &TerminalContext) -> HyperlinkRoute {
    let caps = ctx.hyperlink_capabilities();
    let skip = ctx.hyperlink_skip_reason();

    let emit_osc8 = caps.osc8 == Osc8Support::Native && skip.is_none();
    let emit_id = emit_osc8 && caps.id_param;

    HyperlinkRoute {
        emit_osc8,
        emit_id,
        skip_reason: skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{MultiplexerKind, TerminalContext, TerminalName, TmuxClientMeta};

    fn iterm2_ctx() -> TerminalContext {
        TerminalContext {
            brand: TerminalName::Iterm2,
            ..Default::default()
        }
    }

    fn apple_terminal_ctx() -> TerminalContext {
        TerminalContext {
            brand: TerminalName::AppleTerminal,
            ..Default::default()
        }
    }

    fn warp_ctx() -> TerminalContext {
        TerminalContext {
            brand: TerminalName::WarpTerminal,
            ..Default::default()
        }
    }

    fn iterm2_tmux_34_ctx() -> TerminalContext {
        TerminalContext {
            brand: TerminalName::Iterm2,
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.4".to_owned()),
            tmux_meta: TmuxClientMeta {
                tmux_env: Some("/tmp/tmux-501/default,12345,0".to_owned()),
                tmux_pane: Some("%0".to_owned()),
            },
            ..Default::default()
        }
    }

    fn iterm2_tmux_33_ctx() -> TerminalContext {
        TerminalContext {
            brand: TerminalName::Iterm2,
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.3a".to_owned()),
            tmux_meta: TmuxClientMeta {
                tmux_env: Some("/tmp/tmux-501/default,12345,0".to_owned()),
                tmux_pane: Some("%0".to_owned()),
            },
            ..Default::default()
        }
    }

    fn screen_ctx() -> TerminalContext {
        TerminalContext {
            brand: TerminalName::Iterm2,
            multiplexer: MultiplexerKind::Screen,
            ..Default::default()
        }
    }

    fn vte_old_ctx() -> TerminalContext {
        TerminalContext {
            brand: TerminalName::Vte,
            vte_version: Some("4800".to_owned()),
            ..Default::default()
        }
    }

    fn vte_new_ctx() -> TerminalContext {
        TerminalContext {
            brand: TerminalName::Vte,
            vte_version: Some("7402".to_owned()),
            ..Default::default()
        }
    }

    fn unknown_ctx() -> TerminalContext {
        TerminalContext::default()
    }

    // -- hyperlink_skip_reason tests --

    #[test]
    fn apple_terminal_skip_reason() {
        assert_eq!(
            apple_terminal_ctx().hyperlink_skip_reason(),
            Some("apple_terminal")
        );
    }

    #[test]
    fn warp_skip_reason() {
        assert_eq!(
            warp_ctx().hyperlink_skip_reason(),
            Some("unsupported_terminal")
        );
    }

    #[test]
    fn iterm2_no_skip() {
        assert_eq!(iterm2_ctx().hyperlink_skip_reason(), None);
    }

    #[test]
    fn screen_skip_reason() {
        assert_eq!(screen_ctx().hyperlink_skip_reason(), Some("screen"));
    }

    #[test]
    fn tmux_33_skip_reason() {
        assert_eq!(
            iterm2_tmux_33_ctx().hyperlink_skip_reason(),
            Some("tmux_old")
        );
    }

    #[test]
    fn tmux_34_no_skip() {
        assert_eq!(iterm2_tmux_34_ctx().hyperlink_skip_reason(), None);
    }

    #[test]
    fn vte_old_skip_reason() {
        assert_eq!(vte_old_ctx().hyperlink_skip_reason(), Some("vte_old"));
    }

    #[test]
    fn vte_new_no_skip() {
        assert_eq!(vte_new_ctx().hyperlink_skip_reason(), None);
    }

    #[test]
    fn unknown_terminal_skip_reason() {
        // Unknown brand must report a skip_reason so telemetry/feedback
        // doesn't log "none" alongside emit_osc8=false.
        assert_eq!(
            unknown_ctx().hyperlink_skip_reason(),
            Some("unknown_terminal"),
        );
    }

    #[test]
    fn vte_old_inside_old_tmux_blames_vte() {
        // VTE 0.48 inside tmux 3.2: VTE is the deeper cause, so a tmux
        // upgrade alone wouldn't fix OSC 8. Diagnostic should point at VTE.
        let ctx = TerminalContext {
            brand: TerminalName::Vte,
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.2".to_owned()),
            vte_version: Some("4800".to_owned()),
            ..Default::default()
        };
        assert_eq!(ctx.hyperlink_skip_reason(), Some("vte_old"));
    }

    // -- resolve_hyperlink_route tests --

    #[test]
    fn iterm2_emits_osc8() {
        let route = resolve_hyperlink_route(&iterm2_ctx());
        assert!(route.emit_osc8);
        assert!(route.emit_id);
    }

    #[test]
    fn apple_terminal_fallback() {
        let route = resolve_hyperlink_route(&apple_terminal_ctx());
        assert!(!route.emit_osc8);
        assert_eq!(route.skip_reason, Some("apple_terminal"));
    }

    #[test]
    fn warp_fallback() {
        let route = resolve_hyperlink_route(&warp_ctx());
        assert!(!route.emit_osc8);
    }

    #[test]
    fn tmux_33_fallback() {
        let route = resolve_hyperlink_route(&iterm2_tmux_33_ctx());
        assert!(!route.emit_osc8);
        assert_eq!(route.skip_reason, Some("tmux_old"));
    }

    #[test]
    fn tmux_34_emits() {
        let route = resolve_hyperlink_route(&iterm2_tmux_34_ctx());
        assert!(route.emit_osc8);
        assert!(route.emit_id);
    }

    #[test]
    fn unknown_terminal_no_emit() {
        let route = resolve_hyperlink_route(&unknown_ctx());
        // Unknown brand -> osc8 == Unknown -> not Native -> no emit
        assert!(!route.emit_osc8);
        assert_eq!(route.skip_reason, Some("unknown_terminal"));
    }
}
