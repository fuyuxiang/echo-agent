//! `/remember` -- save a memory note.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Save a memory note inline or enter remember mode.
pub struct RememberCommand;

impl SlashCommand for RememberCommand {
    fn name(&self) -> &str {
        "remember"
    }

    fn description(&self) -> &str {
        "Save a memory note"
    }

    fn usage(&self) -> &str {
        "/remember [text]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("[memory note text]")
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            CommandResult::Action(Action::EnterRememberMode)
        } else {
            CommandResult::Action(Action::SendRememberNote(trimmed.to_string()))
        }
    }
}
