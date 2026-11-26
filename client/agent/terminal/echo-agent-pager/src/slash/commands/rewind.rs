use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct RewindCommand;

impl SlashCommand for RewindCommand {
    fn name(&self) -> &str {
        "rewind"
    }

    fn aliases(&self) -> &[&str] {
        &["undo"]
    }

    fn description(&self) -> &str {
        "Rewind to a previous turn"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/rewind"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::RewindShowPicker)
    }
}
