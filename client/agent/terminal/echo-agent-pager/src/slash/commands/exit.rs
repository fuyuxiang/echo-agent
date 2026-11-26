//! `/quit` (alias `/exit`) -- quit the application.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Bare text that means quit outside slash resolution (`/quit` / `/exit`).
///
/// Shared by agent-prompt send and dashboard dispatch so vim/shell muscle
/// memory (`:wq`, `:q`, `exit`, …) stays one list.
pub(crate) fn is_exit_alias(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "exit" | "quit" | ":q" | ":q!" | ":wq" | ":wq!"
    )
}

/// Quit the pager application.
pub struct ExitCommand;

impl SlashCommand for ExitCommand {
    fn name(&self) -> &str {
        "quit"
    }

    fn aliases(&self) -> &[&str] {
        &["exit"]
    }

    fn description(&self) -> &str {
        "Quit the application"
    }

    fn usage(&self) -> &str {
        "/quit"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::Quit)
    }
}

#[cfg(test)]
mod alias_tests {
    use super::is_exit_alias;

    #[test]
    fn recognizes_shell_and_vim_exit_aliases() {
        for text in [
            "exit", "quit", "EXIT", "Quit", " exit ", ":q", ":Q", ":q!", ":wq", ":WQ", ":wq!",
        ] {
            assert!(is_exit_alias(text), "{text:?} should be an exit alias");
        }
        for text in ["exiting", "quite", ":w", ":x", "q", "wq", "/quit"] {
            assert!(!is_exit_alias(text), "{text:?} must not be an exit alias");
        }
    }
}
