//! `/view-plan` -- open the current saved plan preview.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Open the current session plan preview.
pub struct ViewPlanCommand;

impl SlashCommand for ViewPlanCommand {
    fn name(&self) -> &str {
        "view-plan"
    }

    fn aliases(&self) -> &[&str] {
        &["show-plan", "plan-view"]
    }

    fn description(&self) -> &str {
        "View the current plan"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/view-plan"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::ShowPlan)
    }
}
