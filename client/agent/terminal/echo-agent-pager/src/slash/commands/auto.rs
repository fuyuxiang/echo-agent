//! `/auto` -- toggle auto permission mode (LLM classifier).
//!
//! - Off (or always-approve) → `SetPermissionMode(Auto)`
//! - Already auto → `SetPermissionMode(Ask)` (toggle off)
//!
//! The dispatcher owns state mutation, persistence (with rollback), and toast.
//! Visibility is gated by
//! [`crate::slash::SlashController::set_auto_mode_available`]: `/auto` is
//! hard-hidden when the auto permission-mode feature is off.

use crate::app::actions::{Action, PermissionModeKind};
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Toggle auto permission mode (LLM classifier).
pub struct AutoCommand;

impl SlashCommand for AutoCommand {
    fn name(&self) -> &str {
        "auto"
    }

    fn description(&self) -> &str {
        "Toggle auto mode (classifier approves safe tools)"
    }

    fn usage(&self) -> &str {
        "/auto"
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        // Yolo wins over auto: if always-approve is on, treat auto as off so
        // `/auto` switches into auto rather than "toggling off" to ask.
        let currently_auto = ctx.pager_state.auto_mode && !ctx.pager_state.yolo_mode;
        let kind = if currently_auto {
            PermissionModeKind::Ask
        } else {
            PermissionModeKind::Auto
        };
        CommandResult::Action(Action::SetPermissionMode(kind))
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
        auto_mode: bool,
    ) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot {
                yolo_mode,
                auto_mode,
                auto_mode_gate: true,
                ..PagerLocalSnapshot::default()
            },
        }
    }

    #[test]
    fn off_turns_auto_on() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx(&models, &bundle, false, false);
        assert!(matches!(
            AutoCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::SetPermissionMode(PermissionModeKind::Auto))
        ));
    }

    #[test]
    fn on_turns_auto_off() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx(&models, &bundle, false, true);
        assert!(matches!(
            AutoCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::SetPermissionMode(PermissionModeKind::Ask))
        ));
    }

    #[test]
    fn always_approve_switches_to_auto() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        // Stale auto_mode=true with yolo on must still switch to Auto.
        let mut ctx = make_ctx(&models, &bundle, true, true);
        assert!(matches!(
            AutoCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::SetPermissionMode(PermissionModeKind::Auto))
        ));
    }

    #[test]
    fn ignores_args() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx(&models, &bundle, false, false);
        assert!(matches!(
            AutoCommand.run(&mut ctx, "extra"),
            CommandResult::Action(Action::SetPermissionMode(PermissionModeKind::Auto))
        ));
    }
}
