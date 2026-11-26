//! `/cd [path]` — change the working directory new dashboard sessions
//! spawn in.
//!
//! With no argument it opens the dashboard's location picker; with a path
//! it changes directly. Both are dashboard affordances — invoked from a
//! non-dashboard surface the dispatcher prints a toast pointing the user
//! at `/dashboard` (see `dispatch_dashboard_open_location_picker` /
//! `dispatch_dashboard_change_location`).

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Change the working directory for newly dispatched dashboard sessions.
pub struct CdCommand;

impl SlashCommand for CdCommand {
    fn name(&self) -> &str {
        "cd"
    }

    fn description(&self) -> &str {
        "Change the working directory for new agents"
    }

    fn usage(&self) -> &str {
        "/cd [path]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("path")
    }

    /// `/cd` only makes sense on the dashboard (it changes where the
    /// dashboard dispatches new agents), so hide it from completion on
    /// every other surface — the agent view and the welcome screen.
    fn dashboard_only(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            CommandResult::Action(Action::DashboardOpenLocationPicker)
        } else {
            CommandResult::Action(Action::DashboardChangeLocation {
                input: trimmed.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;

    /// Build a throwaway exec ctx over the given borrows. Mirrors the
    /// inline ctx construction in `dashboard.rs`'s command tests.
    fn ctx<'a>(models: &'a ModelState, bundle: &'a BundleState) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot {
                multiline_mode: false,
                yolo_mode: false,
                ..crate::settings::PagerLocalSnapshot::default()
            },
        }
    }

    #[test]
    fn no_args_opens_location_picker() {
        let (models, bundle) = (ModelState::default(), BundleState::default());
        let mut c = ctx(&models, &bundle);
        assert!(matches!(
            CdCommand.run(&mut c, ""),
            CommandResult::Action(Action::DashboardOpenLocationPicker)
        ));
    }

    #[test]
    fn whitespace_only_opens_location_picker() {
        let (models, bundle) = (ModelState::default(), BundleState::default());
        let mut c = ctx(&models, &bundle);
        assert!(matches!(
            CdCommand.run(&mut c, "   "),
            CommandResult::Action(Action::DashboardOpenLocationPicker)
        ));
    }

    #[test]
    fn path_arg_changes_location() {
        let (models, bundle) = (ModelState::default(), BundleState::default());
        let mut c = ctx(&models, &bundle);
        match CdCommand.run(&mut c, "  ~/projects/foo  ") {
            CommandResult::Action(Action::DashboardChangeLocation { input }) => {
                assert_eq!(input, "~/projects/foo");
            }
            other => panic!("expected DashboardChangeLocation, got {other:?}"),
        }
    }

    #[test]
    fn metadata() {
        let cmd = CdCommand;
        assert_eq!(cmd.name(), "cd");
        assert!(cmd.takes_args());
        assert_eq!(cmd.arg_placeholder(), Some("path"));
        assert!(!cmd.description().is_empty());
        assert!(!cmd.usage().is_empty());
        // `/cd` is dashboard-only — hidden from completion on every other
        // surface (the agent view, the welcome screen).
        assert!(cmd.dashboard_only(), "/cd must be dashboard-only");
    }
}
