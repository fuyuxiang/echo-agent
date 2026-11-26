//! `/settings` -- open the settings modal.
//!
//! No `/settings <id>` direct-jump — args are silently discarded and
//! the modal always opens. Use the in-modal `/` filter to search.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Open the settings modal.
pub struct SettingsCommand;

impl SlashCommand for SettingsCommand {
    fn name(&self) -> &str {
        "settings"
    }

    fn aliases(&self) -> &[&str] {
        &["config", "preferences", "prefs"]
    }

    fn description(&self) -> &str {
        "Open the settings modal"
    }

    fn usage(&self) -> &str {
        "/settings"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenSettings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;

    static DEFAULT_BUNDLE_STATE: crate::app::bundle::BundleState =
        crate::app::bundle::BundleState {
            has_cache: false,
            version: String::new(),
            personas: Vec::new(),
            roles: Vec::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            persona_details: Vec::new(),
            role_details: Vec::new(),
        };

    fn make_ctx<'a>(models: &'a ModelState) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
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
    fn empty_args_dispatches_open_settings() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        let cmd = SettingsCommand;
        let result = cmd.run(&mut ctx, "");
        assert!(
            matches!(result, CommandResult::Action(Action::OpenSettings)),
            "expected OpenSettings, got {result:?}",
        );
    }

    /// Args are silently discarded — modal always opens.
    #[test]
    fn args_still_dispatches_open_settings() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        let cmd = SettingsCommand;
        for args in ["theme", "  ", "anything goes", "compact-mode"] {
            let result = cmd.run(&mut ctx, args);
            assert!(
                matches!(result, CommandResult::Action(Action::OpenSettings)),
                "expected OpenSettings for args={args:?}, got {result:?}",
            );
        }
    }

    #[test]
    fn aliases_are_registered() {
        let cmd = SettingsCommand;
        assert_eq!(cmd.name(), "settings");
        assert_eq!(cmd.aliases(), &["config", "preferences", "prefs"]);
    }
}
