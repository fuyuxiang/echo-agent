use std::io::Write;

use crate::notifications::tmux;
use crate::terminal::{TerminalContext, TerminalName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    Indeterminate,
    Clear,
}

pub fn supports_progress_bar(ctx: &TerminalContext) -> bool {
    match ctx.brand {
        TerminalName::Ghostty | TerminalName::WezTerm => true,
        // iTerm2 added OSC 9;4 progress support in 3.6. Older versions
        // misinterpret the sequence as an OSC 9 desktop notification,
        // displaying the raw parameters (e.g. "4;1;-1") as alert text.
        TerminalName::Iterm2 => ctx.is_term_program_version_or_later(3, 6),
        _ => false,
    }
}

const OSC_INDETERMINATE: &str = "\x1b]9;4;1;-1\x07";
pub(crate) const OSC_CLEAR: &str = "\x1b]9;4;0;0\x07";

/// Build the progress bar escape sequence as an owned `String`.
///
/// Returns `None` if the terminal brand does not support the OSC 9;4 progress
/// indicator.
fn progress_sequence(state: ProgressState, ctx: &TerminalContext) -> Option<String> {
    if !supports_progress_bar(ctx) {
        return None;
    }
    let sequence = match state {
        ProgressState::Indeterminate => OSC_INDETERMINATE,
        ProgressState::Clear => OSC_CLEAR,
    };
    if tmux::passthrough_available(ctx) {
        Some(tmux::tmux_passthrough(sequence))
    } else {
        Some(sequence.to_owned())
    }
}

pub fn emit_progress(state: ProgressState, ctx: &TerminalContext) {
    if let Some(seq) = progress_sequence(state, ctx) {
        echo_agent_runtime::util::with_locked_stderr(|stderr| {
            let _ = stderr.write_all(seq.as_bytes());
            let _ = stderr.flush();
        });
    }
}

/// Build the progress bar escape sequence as a `String` without writing it.
///
/// Returns `None` if the terminal brand does not support the OSC 9;4 progress
/// indicator.
pub fn build_progress_escape(state: ProgressState, ctx: &TerminalContext) -> Option<String> {
    progress_sequence(state, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{MultiplexerKind, TerminalContext};

    fn ctx_for(brand: TerminalName) -> TerminalContext {
        TerminalContext {
            brand,
            ..Default::default()
        }
    }

    #[test]
    fn supported_brands() {
        assert!(supports_progress_bar(&ctx_for(TerminalName::Ghostty)));
        assert!(supports_progress_bar(&ctx_for(TerminalName::WezTerm)));
    }

    #[test]
    fn unsupported_brands() {
        let unsupported = [
            TerminalName::Kitty,
            TerminalName::Alacritty,
            TerminalName::AppleTerminal,
            TerminalName::VsCode,
            TerminalName::WarpTerminal,
            TerminalName::EchoAgentDesktop,
            TerminalName::Vte,
            TerminalName::Unknown,
        ];
        for brand in unsupported {
            assert!(
                !supports_progress_bar(&ctx_for(brand)),
                "{brand:?} should not support progress bar"
            );
        }
    }

    #[test]
    fn emit_noop_for_unsupported_terminal() {
        let ctx = ctx_for(TerminalName::Kitty);
        // Should not panic or write anything meaningful.
        emit_progress(ProgressState::Indeterminate, &ctx);
        emit_progress(ProgressState::Clear, &ctx);
    }

    #[test]
    fn emit_does_not_panic_for_supported_terminals() {
        for ctx in [
            TerminalContext {
                brand: TerminalName::Iterm2,
                term_program_version: Some("3.6.0".into()),
                ..Default::default()
            },
            ctx_for(TerminalName::Ghostty),
            ctx_for(TerminalName::WezTerm),
        ] {
            emit_progress(ProgressState::Indeterminate, &ctx);
            emit_progress(ProgressState::Clear, &ctx);
        }
    }

    #[test]
    fn emit_with_tmux_passthrough() {
        let ctx = TerminalContext {
            brand: TerminalName::Iterm2,
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.3".into()),
            term_program_version: Some("3.6.0".into()),
            ..Default::default()
        };
        // Should not panic; tmux passthrough wrapping is exercised.
        emit_progress(ProgressState::Indeterminate, &ctx);
        emit_progress(ProgressState::Clear, &ctx);
    }

    #[test]
    fn tmux_passthrough_wraps_indeterminate_sequence() {
        use crate::notifications::tmux::tmux_passthrough;
        let wrapped = tmux_passthrough(super::OSC_INDETERMINATE);
        assert_eq!(
            wrapped, "\x1bPtmux;\x1b\x1b]9;4;1;-1\x07\x1b\\",
            "indeterminate sequence should be wrapped with ESC bytes doubled"
        );
    }

    #[test]
    fn tmux_passthrough_wraps_clear_sequence() {
        use crate::notifications::tmux::tmux_passthrough;
        let wrapped = tmux_passthrough(super::OSC_CLEAR);
        assert_eq!(
            wrapped, "\x1bPtmux;\x1b\x1b]9;4;0;0\x07\x1b\\",
            "clear sequence should be wrapped with ESC bytes doubled"
        );
    }

    // --- build_progress_escape tests ---

    #[test]
    fn build_returns_none_for_unsupported_brand() {
        let ctx = ctx_for(TerminalName::Kitty);
        assert!(build_progress_escape(ProgressState::Indeterminate, &ctx).is_none());
        assert!(build_progress_escape(ProgressState::Clear, &ctx).is_none());
    }

    #[test]
    fn build_returns_indeterminate_for_new_iterm2() {
        let ctx = TerminalContext {
            brand: TerminalName::Iterm2,
            term_program_version: Some("3.6.0".into()),
            ..Default::default()
        };
        assert_eq!(
            build_progress_escape(ProgressState::Indeterminate, &ctx).as_deref(),
            Some(super::OSC_INDETERMINATE),
        );
    }

    #[test]
    fn build_returns_clear_for_supported_brand() {
        let ctx = ctx_for(TerminalName::Ghostty);
        assert_eq!(
            build_progress_escape(ProgressState::Clear, &ctx).as_deref(),
            Some(super::OSC_CLEAR),
        );
    }

    #[test]
    fn build_wraps_with_tmux_passthrough() {
        let ctx = TerminalContext {
            brand: TerminalName::Iterm2,
            multiplexer: MultiplexerKind::Tmux,
            tmux_version: Some("tmux 3.3".into()),
            term_program_version: Some("3.6.0".into()),
            ..Default::default()
        };
        let result = build_progress_escape(ProgressState::Indeterminate, &ctx).unwrap();
        assert!(
            result.starts_with("\x1bPtmux;"),
            "expected tmux passthrough wrapper, got: {result:?}",
        );
        assert!(
            result.ends_with("\x1b\\"),
            "expected ST terminator, got: {result:?}",
        );
    }
}
