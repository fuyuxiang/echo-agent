//! `/personas` -- open the agents modal on the Personas tab.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
use crate::views::agents_modal::AgentsTab;

/// Open the agents modal directly on the Personas tab.
pub struct PersonasCommand;

impl SlashCommand for PersonasCommand {
    fn name(&self) -> &str {
        "personas"
    }

    fn aliases(&self) -> &[&str] {
        &[]
    }

    fn description(&self) -> &str {
        "Manage personas (create, edit, delete)"
    }

    fn usage(&self) -> &str {
        "/personas"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenConfigAgentsModal(Some(AgentsTab::Personas)))
    }
}
