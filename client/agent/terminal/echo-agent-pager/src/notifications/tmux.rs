pub use echo_agent_pager_render::terminal::{
    passthrough_available, tmux_passthrough_str as tmux_passthrough,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{MultiplexerKind, TerminalContext};

    #[test]
    fn passthrough_wraps_osc9_sequence() {
        let seq = "\x1b]9;task done\x07";
        let wrapped = tmux_passthrough(seq);
        // ESC bytes doubled, wrapped in DCS tmux; ... ST
        assert_eq!(wrapped, "\x1bPtmux;\x1b\x1b]9;task done\x07\x1b\\");
    }

    #[test]
    fn passthrough_wraps_osc777_with_st_terminator() {
        let seq = "\x1b]777;notify;title;body\x1b\\";
        let wrapped = tmux_passthrough(seq);
        assert_eq!(
            wrapped,
            "\x1bPtmux;\x1b\x1b]777;notify;title;body\x1b\x1b\\\x1b\\"
        );
    }

    #[test]
    fn passthrough_no_esc_in_input() {
        let seq = "plain text";
        let wrapped = tmux_passthrough(seq);
        assert_eq!(wrapped, "\x1bPtmux;plain text\x1b\\");
    }

    #[test]
    fn passthrough_bel_has_no_esc_to_double() {
        let seq = "\x07";
        let wrapped = tmux_passthrough(seq);
        assert_eq!(wrapped, "\x1bPtmux;\x07\x1b\\");
    }

    #[test]
    fn passthrough_available_tmux_3_3() {
        let ctx = TerminalContext {
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.3".into()),
            ..Default::default()
        };
        assert!(passthrough_available(&ctx));
    }

    #[test]
    fn passthrough_available_tmux_3_4() {
        let ctx = TerminalContext {
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.4".into()),
            ..Default::default()
        };
        assert!(passthrough_available(&ctx));
    }

    #[test]
    fn passthrough_unavailable_tmux_3_2() {
        let ctx = TerminalContext {
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.2".into()),
            ..Default::default()
        };
        assert!(!passthrough_available(&ctx));
    }

    #[test]
    fn passthrough_unavailable_no_tmux() {
        let ctx = TerminalContext::default();
        assert!(!passthrough_available(&ctx));
    }

    #[test]
    fn passthrough_unavailable_tmux_no_version() {
        let ctx = TerminalContext {
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: None,
            ..Default::default()
        };
        assert!(!passthrough_available(&ctx));
    }
}
