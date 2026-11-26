//! `/workflows` -- browse the workflow catalog in the extensions modal.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
use crate::views::extensions_modal::ExtensionsTab;
use echo_agent_telemetry::events::ExtensionsModalTrigger;

/// Open the extensions modal on the Workflows catalog tab.
pub struct WorkflowsCommand;

impl SlashCommand for WorkflowsCommand {
    fn name(&self) -> &str {
        "workflows"
    }

    fn description(&self) -> &str {
        "Browse installed workflows"
    }

    fn usage(&self) -> &str {
        "/workflows"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenExtensionsModal {
            tab: ExtensionsTab::Workflows,
            trigger: ExtensionsModalTrigger::SlashCommand,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;
    use crate::slash::ModeSupport;

    static DEFAULT_BUNDLE_STATE: BundleState = BundleState {
        has_cache: false,
        version: String::new(),
        personas: Vec::new(),
        roles: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
        persona_details: Vec::new(),
        role_details: Vec::new(),
    };

    #[test]
    fn visibility_is_defensive_during_catalog_reload() {
        let models = ModelState::default();
        for available in [false, true] {
            let ctx = crate::slash::command::AppCtx {
                models: &models,
                cwd: std::path::Path::new("."),
                has_session_announcements: false,
                billing_surface_visible: true,
                usage_command_visible: true,
                workflows_available: available,
                saved_workflows: &[],
                workflow_runs: &[],
                screen_mode: crate::app::ScreenMode::Fullscreen,
                current_title: None,
            };
            assert!(WorkflowsCommand.visible(&ctx));
        }
    }

    #[test]
    fn workflows_opens_catalog_tab_in_both_modes() {
        let models = ModelState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Minimal,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        };
        assert!(matches!(
            WorkflowsCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::OpenExtensionsModal {
                tab: ExtensionsTab::Workflows,
                trigger: ExtensionsModalTrigger::SlashCommand,
            })
        ));
        // Parity with /skills: the modal renders in minimal mode too.
        assert!(matches!(WorkflowsCommand.mode_support(), ModeSupport::Both));
    }
}
