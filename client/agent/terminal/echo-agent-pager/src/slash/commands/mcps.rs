use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct McpsCommand;

impl SlashCommand for McpsCommand {
    fn name(&self) -> &str {
        "mcps"
    }

    fn description(&self) -> &str {
        "Show MCP server status"
    }

    fn usage(&self) -> &str {
        "/mcps"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenExtensionsModal {
            tab: crate::views::extensions_modal::ExtensionsTab::McpServers,
            trigger: echo_agent_telemetry::events::ExtensionsModalTrigger::SlashCommand,
        })
    }
}
