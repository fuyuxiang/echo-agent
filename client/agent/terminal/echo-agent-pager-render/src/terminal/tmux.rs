//! tmux DCS passthrough wrapping.
//!
//! Callers must pass an already-sanitized payload. These helpers only double
//! ESC (`0x1b`); they do not strip CAN (`0x18`), SUB (`0x1a`), C1 ST (`0x9c`),
//! or BEL (`0x07`), any of which abort or terminate DCS/OSC in tmux/xterm.

use super::TerminalContext;

/// Wrap `sequence` in `\x1bPtmux;…\x1b\\`, doubling embedded ESC bytes.
#[must_use]
pub fn tmux_passthrough(sequence: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(sequence.len() + 16);
    out.extend_from_slice(b"\x1bPtmux;");
    for &byte in sequence {
        if byte == 0x1b {
            out.push(0x1b);
        }
        out.push(byte);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// String form of [`tmux_passthrough`]. Builds the envelope in UTF-8; only
/// ASCII is inserted and ESC doubling cannot split a multibyte sequence.
#[must_use]
pub fn tmux_passthrough_str(sequence: &str) -> String {
    let mut out = String::with_capacity(sequence.len() + 16);
    out.push_str("\x1bPtmux;");
    for ch in sequence.chars() {
        if ch == '\x1b' {
            out.push('\x1b');
        }
        out.push(ch);
    }
    out.push_str("\x1b\\");
    out
}

/// tmux ≥ 3.3 as the immediate emulator — minimum for reliable DCS passthrough.
#[must_use]
pub fn passthrough_available(ctx: &TerminalContext) -> bool {
    ctx.is_tmux_backed() && ctx.is_tmux_version_or_later(3, 3)
}

/// OSC 11 wrap: passthrough-capable tmux that is not an editor `:terminal`.
#[must_use]
pub fn should_wrap_osc11(ctx: &TerminalContext) -> bool {
    passthrough_available(ctx) && ctx.embedded_editor.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{EmbeddedEditor, MultiplexerKind, TerminalContext};

    #[test]
    fn doubles_esc_and_wraps() {
        assert_eq!(
            tmux_passthrough(b"\x1b]11;?\x07"),
            b"\x1bPtmux;\x1b\x1b]11;?\x07\x1b\\"
        );
    }

    #[test]
    fn plain_text_has_no_esc_to_double() {
        assert_eq!(tmux_passthrough(b"plain"), b"\x1bPtmux;plain\x1b\\");
    }

    #[test]
    fn doubles_every_esc_including_st() {
        assert_eq!(
            tmux_passthrough(b"\x1b]777;notify;t;b\x1b\\"),
            b"\x1bPtmux;\x1b\x1b]777;notify;t;b\x1b\x1b\\\x1b\\"
        );
    }

    #[test]
    fn str_wrapper_matches_byte_wrapper() {
        let seq = "\x1b]9;task done\x07";
        assert_eq!(
            tmux_passthrough_str(seq).as_bytes(),
            tmux_passthrough(seq.as_bytes())
        );
        assert_eq!(tmux_passthrough_str("plain"), "\x1bPtmux;plain\x1b\\");
    }

    #[test]
    fn passthrough_available_requires_tmux_33() {
        let tmux_33 = TerminalContext {
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.3".into()),
            ..Default::default()
        };
        assert!(passthrough_available(&tmux_33));
        assert!(should_wrap_osc11(&tmux_33));

        let tmux_32 = TerminalContext {
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.2".into()),
            ..Default::default()
        };
        assert!(!passthrough_available(&tmux_32));
        assert!(!should_wrap_osc11(&tmux_32));

        let tmux_version_unknown = TerminalContext {
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: None,
            ..Default::default()
        };
        assert!(!passthrough_available(&tmux_version_unknown));

        assert!(!passthrough_available(&TerminalContext::default()));
    }

    #[test]
    fn should_wrap_osc11_skips_embedded_editor() {
        let nvim = TerminalContext {
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.4".into()),
            embedded_editor: Some(EmbeddedEditor::Neovim),
            ..Default::default()
        };
        assert!(passthrough_available(&nvim));
        assert!(!should_wrap_osc11(&nvim));
    }
}
