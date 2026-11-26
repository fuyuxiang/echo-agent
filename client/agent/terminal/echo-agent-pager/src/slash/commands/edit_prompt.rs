//! `/edit-prompt` -- edit the composer in an external editor.
//!
//! The full TUI's only route to the editor (`Ctrl+G` stays with the tasks
//! pane there); in minimal mode it doubles as the fallback for terminals
//! that reserve the `Ctrl+G` chord.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct EditPromptCommand;

impl SlashCommand for EditPromptCommand {
    fn name(&self) -> &str {
        "edit-prompt"
    }

    fn description(&self) -> &str {
        "Open an external editor for an empty prompt; use the command palette to preserve a draft"
    }

    fn usage(&self) -> &str {
        "/edit-prompt"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error("No active session".to_owned());
        }
        CommandResult::Action(Action::EditPromptExternal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

    fn exec_ctx<'a>(
        models: &'a ModelState,
        bundle: &'a BundleState,
        session_id: Option<&'a agent_client_protocol::SessionId>,
        mode: crate::app::ScreenMode,
    ) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id,
            bundle_state: bundle,
            screen_mode: mode,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn opens_the_external_editor_in_both_modes() {
        let command = EditPromptCommand;
        let models = ModelState::default();
        let bundle = BundleState::default();
        let session_id = agent_client_protocol::SessionId::from("session".to_owned());

        for mode in [
            crate::app::ScreenMode::Minimal,
            crate::app::ScreenMode::Fullscreen,
        ] {
            assert!(matches!(
                command.run(&mut exec_ctx(&models, &bundle, Some(&session_id), mode), ""),
                CommandResult::Action(Action::EditPromptExternal)
            ));
        }
    }

    #[test]
    fn requires_session() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        assert!(matches!(
            EditPromptCommand.run(
                &mut exec_ctx(
                    &models,
                    &bundle,
                    None,
                    crate::app::ScreenMode::Minimal,
                ),
                "",
            ),
            CommandResult::Error(message) if message.contains("No active session")
        ));
    }
}
