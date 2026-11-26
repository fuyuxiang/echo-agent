//! Detects whether echoagent is running inside an editor's embedded `:terminal`
//! (Neovim/Vim `:terminal`, Emacs `vterm`).
//!
//! WHY this matters: inside an editor `:terminal` the *immediate* terminal
//! emulator is the editor's own libvterm, not tmux — even though the `TMUX`
//! env var is inherited through the editor. A tmux DCS passthrough envelope
//! (`\x1bPtmux;…\x1b\\`) is only understood by tmux, so emitting it into the
//! editor's libvterm renders the wrapper as visible garbage text. Detection
//! records this on [`super::TerminalContext`] so clipboard routing emits a
//! plain OSC 52 sequence in that case instead.

use std::collections::HashMap;

use super::env_get;

/// Which embedded editor `:terminal` echoagent is running inside.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddedEditor {
    /// Neovim `:terminal` (sets `NVIM`, or legacy `NVIM_LISTEN_ADDRESS`).
    Neovim,
    /// Vim 8/9 `:terminal` (sets `VIM_TERMINAL`).
    Vim,
    /// Emacs (sets `INSIDE_EMACS`; `vterm` uses libvterm — same bug).
    Emacs,
}

/// Detect the embedded editor terminal from an injected environment map.
///
/// Checks, in order: `NVIM` / `NVIM_LISTEN_ADDRESS` → [`EmbeddedEditor::Neovim`];
/// `VIM_TERMINAL` → [`EmbeddedEditor::Vim`]; `INSIDE_EMACS` →
/// [`EmbeddedEditor::Emacs`]; else `None`. Empty values are treated as absent
/// (matching the sibling `detect_*_from_env` detectors via `env_get`).
///
/// Adding a new env marker here requires extending
/// `HOST_TERMINAL_ENV_VARS` in `echo-agent-pager-pty-harness/src/pty.rs`
/// (test-env hygiene).
pub fn embedded_editor_from_env(env: &HashMap<String, String>) -> Option<EmbeddedEditor> {
    // Markers can't distinguish editor-inside-tmux (the 100%-repro bug; don't wrap)
    // from the inverted tmux-inside-editor; we target the former and the latter still
    // works since plain OSC 52 is forwarded.
    // NVIM_LISTEN_ADDRESS is legacy (modern nvim unsets it at startup); a
    // stray/user-exported marker only degrades to plain OSC 52, never garbage.
    if env_get(env, "NVIM").is_some() || env_get(env, "NVIM_LISTEN_ADDRESS").is_some() {
        return Some(EmbeddedEditor::Neovim);
    }
    if env_get(env, "VIM_TERMINAL").is_some() {
        return Some(EmbeddedEditor::Vim);
    }
    if env_get(env, "INSIDE_EMACS").is_some() {
        return Some(EmbeddedEditor::Emacs);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::env_from;

    #[test]
    fn nvim_detected_as_neovim() {
        let env = env_from(&[("NVIM", "/tmp/nvim.12345.0")]);
        assert_eq!(embedded_editor_from_env(&env), Some(EmbeddedEditor::Neovim));
    }

    #[test]
    fn nvim_listen_address_detected_as_neovim() {
        let env = env_from(&[("NVIM_LISTEN_ADDRESS", "/tmp/nvim.sock")]);
        assert_eq!(embedded_editor_from_env(&env), Some(EmbeddedEditor::Neovim));
    }

    #[test]
    fn vim_terminal_detected_as_vim() {
        let env = env_from(&[("VIM_TERMINAL", "8.2")]);
        assert_eq!(embedded_editor_from_env(&env), Some(EmbeddedEditor::Vim));
    }

    #[test]
    fn inside_emacs_detected_as_emacs() {
        let env = env_from(&[("INSIDE_EMACS", "30.1,vterm")]);
        assert_eq!(embedded_editor_from_env(&env), Some(EmbeddedEditor::Emacs));
    }

    #[test]
    fn no_editor_markers_is_none() {
        let env = env_from(&[
            ("TERM", "xterm-256color"),
            ("TMUX", "/tmp/tmux-501/default,1,0"),
        ]);
        assert_eq!(embedded_editor_from_env(&env), None);
    }

    #[test]
    fn empty_value_treated_as_absent() {
        let env = env_from(&[("NVIM", ""), ("VIM_TERMINAL", ""), ("INSIDE_EMACS", "")]);
        assert_eq!(embedded_editor_from_env(&env), None);
    }

    #[test]
    fn nvim_beats_vim_and_emacs() {
        let env = env_from(&[
            ("NVIM", "/tmp/nvim.12345.0"),
            ("VIM_TERMINAL", "8.2"),
            ("INSIDE_EMACS", "30.1,vterm"),
        ]);
        assert_eq!(embedded_editor_from_env(&env), Some(EmbeddedEditor::Neovim));
    }
}
