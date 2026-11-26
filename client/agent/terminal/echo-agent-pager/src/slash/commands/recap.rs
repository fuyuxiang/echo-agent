//! `/recap` (alias `/summarize`) -- summarize the session so far ("where was I").
//!
//! Returns `CommandResult::Action(Action::SendRecap { auto: false })` so the
//! dispatch layer fires it as an ACP ext method (`echo.agent/recap`) that bypasses
//! the prompt queue. The recap arrives asynchronously as a scrollback line and
//! is never added to the model conversation.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct RecapCommand;

impl SlashCommand for RecapCommand {
    fn name(&self) -> &str {
        "recap"
    }

    fn aliases(&self) -> &[&str] {
        &["summarize"]
    }

    fn description(&self) -> &str {
        "Summarize the session so far"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/recap"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::SendRecap { auto: false })
    }
}
