//! `/config-agents` -- open the agents modal.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Open the agents modal listing all agent definitions.
pub struct ConfigAgentsCommand;

impl SlashCommand for ConfigAgentsCommand {
    fn name(&self) -> &str {
        "config-agents"
    }

    fn aliases(&self) -> &[&str] {
        &["agents"]
    }

    fn description(&self) -> &str {
        "Manage agent definitions"
    }

    fn usage(&self) -> &str {
        "/config-agents"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenConfigAgentsModal(None))
    }
}
