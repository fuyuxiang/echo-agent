//! `/always-approve` -- toggle auto-approve (YOLO / `permission_mode`).
//!
//! Dispatches `Action::SetYoloMode(!current)`. The dispatcher handles
//! state mutation, permission_queue drain, persistence (with rollback
//! on disk-write failure), and toast.
//!
//! No scrollback turn — visible effects are the prompt-line chip and
//! a toast (destructive-styled when enabling).

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Toggle always-approve (YOLO / `permission_mode`).
pub struct AlwaysApproveCommand;

impl SlashCommand for AlwaysApproveCommand {
    fn name(&self) -> &str {
        "always-approve"
    }

    fn description(&self) -> &str {
        "Toggle always-approve mode (skip all permission prompts)"
    }

    fn usage(&self) -> &str {
        "/always-approve"
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        let new = !ctx.pager_state.yolo_mode;
        CommandResult::Action(Action::SetYoloMode(new))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

    fn make_ctx<'a>(
        models: &'a ModelState,
        bundle: &'a BundleState,
        yolo_mode: bool,
    ) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot {
                multiline_mode: false,
                yolo_mode,
                ..PagerLocalSnapshot::default()
            },
        }
    }

    #[test]
    fn off_turns_always_approve_on() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx(&models, &bundle, false);
        assert!(matches!(
            AlwaysApproveCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::SetYoloMode(true))
        ));
    }

    #[test]
    fn on_turns_always_approve_off() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx(&models, &bundle, true);
        assert!(matches!(
            AlwaysApproveCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::SetYoloMode(false))
        ));
    }

    #[test]
    fn ignores_args() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx(&models, &bundle, false);
        assert!(matches!(
            AlwaysApproveCommand.run(&mut ctx, "extra"),
            CommandResult::Action(Action::SetYoloMode(true))
        ));
    }
}
