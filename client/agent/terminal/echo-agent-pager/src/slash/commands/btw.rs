//! `/btw` -- ask a side question without interrupting the running agent.
//!
//! Returns `CommandResult::Action(Action::SendBtw(...))` so the dispatch layer
//! fires it as an ACP ext method (`echo.agent/btw`) that bypasses the prompt queue.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct BtwCommand;

impl SlashCommand for BtwCommand {
    fn name(&self) -> &str {
        "btw"
    }

    fn description(&self) -> &str {
        "Ask a side question without interrupting"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/btw <question>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<question>")
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        CommandResult::Action(Action::SendBtw(args.trim().to_string()))
    }
}
