//! `/vim-mode` -- toggle vim-style scrollback keybindings.
//!
//! When off (default), bare-letter and Shift+letter keys in the scrollback
//! (j/k, h/l, g/G, y/Y, o/O, r, x, e/E, L/H, plus the `i` insert alt)
//! are suppressed and instead jump focus to the prompt so the letter is
//! typed into the textarea. Arrow/Tab/Esc/Space/PgUp/PgDn and all
//! Ctrl+letter bindings stay active in both modes.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Toggle vim-style scrollback keybindings via `/vim-mode`.
pub struct VimModeCommand;

impl SlashCommand for VimModeCommand {
    fn name(&self) -> &str {
        "vim-mode"
    }

    fn description(&self) -> &str {
        "Toggle vim-style scrollback keybindings (j/k, h/l, g/G, y/Y, …)"
    }

    fn usage(&self) -> &str {
        "/vim-mode"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::ToggleVimMode)
    }
}
