//! `/plan` -- enter plan mode.
//!
//! `/plan` enters plan mode. `/plan <description>` enters plan mode and starts
//! a turn with the description after the mode switch completes.
//!
//! Use `/view-plan` to open the current saved plan preview.

use crate::app::actions::{Action, PlanModeKind};
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Enter plan mode.
pub struct PlanCommand;

impl SlashCommand for PlanCommand {
    fn name(&self) -> &str {
        "plan"
    }

    fn description(&self) -> &str {
        "Enter plan mode"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn offered_when_session_less(&self) -> bool {
        // The dashboard offers `/plan` to start the next spawned agent in
        // plan mode (intercepted in `dispatch_dashboard_dispatch_slash`).
        true
    }

    fn usage(&self) -> &str {
        "/plan [description]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("[description]")
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return CommandResult::Action(Action::SetPlanMode(PlanModeKind::On));
        }
        CommandResult::Action(Action::EnterPlanMode {
            description: Some(trimmed.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

    fn make_ctx_inactive_plan_mode<'a>(
        models: &'a ModelState,
        bundle: &'a BundleState,
    ) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot {
                plan_mode_active: false,
                ..PagerLocalSnapshot::default()
            },
        }
    }

    fn make_ctx_active_plan_mode<'a>(
        models: &'a ModelState,
        bundle: &'a BundleState,
    ) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot {
                plan_mode_active: true,
                ..PagerLocalSnapshot::default()
            },
        }
    }

    /// `/plan` (no args, not in plan mode) → `SetPlanMode(On)`.
    #[test]
    fn no_args_not_in_plan_dispatches_set_plan_mode_on() {
        let cmd = PlanCommand;
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx_inactive_plan_mode(&models, &bundle);
        match cmd.run(&mut ctx, "") {
            CommandResult::Action(Action::SetPlanMode(kind)) => {
                assert_eq!(
                    kind,
                    PlanModeKind::On,
                    "`/plan` (no args, not in plan mode) must dispatch SetPlanMode(On)"
                );
            }
            other => panic!("expected Action::SetPlanMode, got {other:?}"),
        }
    }

    /// `/plan` (no args, already in plan mode) → idempotent `SetPlanMode(On)`.
    #[test]
    fn no_args_already_in_plan_dispatches_set_plan_mode_on() {
        let cmd = PlanCommand;
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx_active_plan_mode(&models, &bundle);
        match cmd.run(&mut ctx, "") {
            CommandResult::Action(Action::SetPlanMode(kind)) => {
                assert_eq!(kind, PlanModeKind::On);
            }
            other => panic!("expected Action::SetPlanMode, got {other:?}"),
        }
    }

    /// Whitespace-only → treated as no args.
    #[test]
    fn whitespace_only_arg_not_in_plan_dispatches_set_plan_mode_on() {
        let cmd = PlanCommand;
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx_inactive_plan_mode(&models, &bundle);
        match cmd.run(&mut ctx, "   ") {
            CommandResult::Action(Action::SetPlanMode(kind)) => {
                assert_eq!(kind, PlanModeKind::On);
            }
            other => panic!("expected SetPlanMode for whitespace-only arg, got {other:?}"),
        }
    }

    /// `/plan <description>` → `EnterPlanMode` with description.
    #[test]
    fn with_description_keeps_enter_plan_mode_when_not_in_plan() {
        let cmd = PlanCommand;
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx_inactive_plan_mode(&models, &bundle);
        match cmd.run(&mut ctx, "Refactor the auth flow") {
            CommandResult::Action(Action::EnterPlanMode { description }) => {
                assert_eq!(
                    description.as_deref(),
                    Some("Refactor the auth flow"),
                    "`/plan <desc>` must dispatch EnterPlanMode with the description"
                );
            }
            other => panic!("expected Action::EnterPlanMode, got {other:?}"),
        }
    }

    /// `/plan <description>` when already in plan mode still emits
    /// `EnterPlanMode`; the dispatcher owns the idempotent mode handling.
    #[test]
    fn with_description_already_in_plan_keeps_enter_plan_mode() {
        let cmd = PlanCommand;
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx_active_plan_mode(&models, &bundle);
        match cmd.run(&mut ctx, "something") {
            CommandResult::Action(Action::EnterPlanMode { description }) => {
                assert_eq!(description.as_deref(), Some("something"));
            }
            other => panic!("expected EnterPlanMode, got {other:?}"),
        }
    }

    /// Whitespace is trimmed from the description.
    #[test]
    fn with_description_trims_whitespace() {
        let cmd = PlanCommand;
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx_inactive_plan_mode(&models, &bundle);
        match cmd.run(&mut ctx, "  hello world  ") {
            CommandResult::Action(Action::EnterPlanMode { description }) => {
                assert_eq!(description.as_deref(), Some("hello world"));
            }
            other => panic!("expected EnterPlanMode, got {other:?}"),
        }
    }
}
