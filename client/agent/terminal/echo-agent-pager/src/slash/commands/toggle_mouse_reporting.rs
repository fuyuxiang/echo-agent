//! `/toggle-mouse-reporting` — flip terminal mouse capture from anywhere.
//!
//! Opt-in companion to the `Ctrl+R` (scrollback-focused) shortcut. Disabling
//! capture hands mouse selection back to the terminal for native click-drag
//! copy/paste; re-enabling restores in-app mouse support. Unlike the keybinding,
//! the command runs from the prompt or scrollback without defocusing input.
//!
//! Gated on `[ui] mouse_reporting_toggle = true` (cached at startup in
//! [`crate::app::mouse_reporting_toggle_enabled`]): hidden from the dropdown and
//! inert (prints a hint) when the feature is off.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, CommandExecCtx, CommandResult, SlashCommand};

/// Toggle terminal mouse reporting (mouse capture). Mirrors the `Ctrl+R`
/// scrollback shortcut via the same `Action::ToggleMouseCapture` path.
pub struct ToggleMouseReportingCommand;

impl SlashCommand for ToggleMouseReportingCommand {
    fn name(&self) -> &str {
        "toggle-mouse-reporting"
    }

    fn description(&self) -> &str {
        "Toggle terminal mouse reporting (native click-drag copy/paste)"
    }

    fn usage(&self) -> &str {
        "/toggle-mouse-reporting"
    }

    /// Only offered when the opt-in feature is enabled in config.
    fn visible(&self, _ctx: &AppCtx) -> bool {
        crate::app::mouse_reporting_toggle_enabled()
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        if crate::app::mouse_reporting_toggle_enabled() {
            CommandResult::Action(Action::ToggleMouseCapture)
        } else {
            CommandResult::Message(
                "Mouse reporting toggle is off. Set `[ui] mouse_reporting_toggle = true` \
                 in ~/.echo-agent/config.toml to enable it."
                    .to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use std::sync::atomic::Ordering;

    fn set_enabled(on: bool) {
        crate::app::MOUSE_REPORTING_TOGGLE_ENABLED.store(on, Ordering::Release);
    }

    fn exec_ctx<'a>(models: &'a ModelState, bundle: &'a BundleState) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    #[serial_test::serial(MOUSE_REPORTING_TOGGLE_ENABLED)]
    #[test]
    fn run_returns_toggle_action_when_enabled() {
        set_enabled(true);
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = exec_ctx(&models, &bundle);
        assert!(matches!(
            ToggleMouseReportingCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::ToggleMouseCapture)
        ));
        set_enabled(false);
    }

    #[serial_test::serial(MOUSE_REPORTING_TOGGLE_ENABLED)]
    #[test]
    fn run_returns_hint_message_when_disabled() {
        set_enabled(false);
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = exec_ctx(&models, &bundle);
        assert!(matches!(
            ToggleMouseReportingCommand.run(&mut ctx, ""),
            CommandResult::Message(_)
        ));
    }

    #[serial_test::serial(MOUSE_REPORTING_TOGGLE_ENABLED)]
    #[test]
    fn visible_tracks_config_flag() {
        let models = ModelState::default();
        let ctx = AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            billing_surface_visible: true,
            usage_command_visible: true,
            workflows_available: true,
            saved_workflows: &[],
            workflow_runs: &[],
            screen_mode: crate::app::ScreenMode::Fullscreen,
            current_title: None,
        };
        set_enabled(true);
        assert!(ToggleMouseReportingCommand.visible(&ctx));
        set_enabled(false);
        assert!(!ToggleMouseReportingCommand.visible(&ctx));
    }
}
