//! `/hooks` and `/plugins` -- open the hooks/plugins modal.
//!
//! These commands always open the tabbed modal. All hook/plugin management
//! (install, uninstall, trust, etc.) is done through the modal's UI — no
//! subcommands are passed through to the shell via the slash command.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
use crate::views::extensions_modal::ExtensionsTab;
use echo_agent_telemetry::events::ExtensionsModalTrigger;

/// Open the hooks/plugins modal on the Hooks tab.
pub struct HooksCommand;

impl SlashCommand for HooksCommand {
    fn name(&self) -> &str {
        "hooks"
    }

    fn description(&self) -> &str {
        "View hooks"
    }

    fn usage(&self) -> &str {
        "/hooks"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenExtensionsModal {
            tab: ExtensionsTab::Hooks,
            trigger: ExtensionsModalTrigger::SlashCommand,
        })
    }
}

/// Open the hooks/plugins modal on the Plugins tab.
pub struct PluginsCommand;

impl SlashCommand for PluginsCommand {
    fn name(&self) -> &str {
        "plugins"
    }

    fn aliases(&self) -> &[&str] {
        &["plugin"]
    }

    fn description(&self) -> &str {
        "View plugins"
    }

    fn usage(&self) -> &str {
        "/plugins"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenExtensionsModal {
            tab: ExtensionsTab::Plugins,
            trigger: ExtensionsModalTrigger::SlashCommand,
        })
    }
}

/// Open the hooks/plugins modal on the Marketplace tab.
pub struct MarketplaceCommand;

impl SlashCommand for MarketplaceCommand {
    fn name(&self) -> &str {
        "marketplace"
    }

    fn description(&self) -> &str {
        "View marketplace"
    }

    fn usage(&self) -> &str {
        "/marketplace"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenExtensionsModal {
            tab: ExtensionsTab::Marketplace,
            trigger: ExtensionsModalTrigger::SlashCommand,
        })
    }
}

/// Open the hooks/plugins modal on the Skills tab.
pub struct SkillsCommand;

impl SlashCommand for SkillsCommand {
    fn name(&self) -> &str {
        "skills"
    }

    fn description(&self) -> &str {
        "View skills"
    }

    fn usage(&self) -> &str {
        "/skills"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenExtensionsModal {
            tab: ExtensionsTab::Skills,
            trigger: ExtensionsModalTrigger::SlashCommand,
        })
    }
}
