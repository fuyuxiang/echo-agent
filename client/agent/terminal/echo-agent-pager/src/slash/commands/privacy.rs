//! `/privacy` -- open the "Coding data, retention, and training" setting.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

const CODING_DATA_SHARING_KEY: &str = "coding_data_sharing";

/// Open settings on `coding_data_sharing`. Takes no arguments.
pub struct PrivacyCommand;

impl SlashCommand for PrivacyCommand {
    fn name(&self) -> &str {
        "privacy"
    }

    fn description(&self) -> &str {
        // Reads as the row it opens: "Coding data, retention, and training".
        "Open coding data, retention, and training settings"
    }

    fn usage(&self) -> &str {
        "/privacy"
    }

    /// Trailing text is ignored, not rejected: `/privacy opt-in` from muscle
    /// memory should land on the page, not error.
    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenSettingsFocus {
            key: CODING_DATA_SHARING_KEY,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `/privacy <args>` in `mode`.
    fn run_privacy(args: &str, mode: crate::app::ScreenMode) -> CommandResult {
        use crate::acp::model_state::ModelState;
        use crate::app::bundle::BundleState;

        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &bundle,
            screen_mode: mode,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        };
        PrivacyCommand.run(&mut ctx, args)
    }

    fn opens_settings_row(result: &CommandResult) -> bool {
        matches!(
            result,
            CommandResult::Action(Action::OpenSettingsFocus {
                key: CODING_DATA_SHARING_KEY
            })
        )
    }

    /// Minimal suppresses the privacy banner, so `/privacy` is the only
    /// route to the page there — no mode may fall back to something else.
    #[test]
    fn privacy_opens_settings_row_in_every_screen_mode() {
        use crate::app::ScreenMode;
        for mode in [
            ScreenMode::Fullscreen,
            ScreenMode::Inline,
            ScreenMode::Minimal,
        ] {
            let result = run_privacy("", mode);
            assert!(
                opens_settings_row(&result),
                "`/privacy` in {mode:?} must open the settings row, got {result:?}",
            );
        }
    }

    /// The arguments this used to accept must not linger as hidden aliases
    /// that change a privacy preference straight from the prompt.
    #[test]
    fn arguments_are_ignored_not_honored() {
        use crate::app::ScreenMode;
        assert!(
            !PrivacyCommand.takes_args(),
            "the dropdown must not offer an argument slot"
        );
        for args in [
            "   ", "opt-in", "opt-out", "in", "out", "share", "private", "status", "info",
            "garbage",
        ] {
            let result = run_privacy(args, ScreenMode::Inline);
            assert!(
                opens_settings_row(&result),
                "`/privacy {args}` must just open the page, got {result:?}",
            );
        }
    }
}
