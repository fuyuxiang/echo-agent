//! `/tutorial` -- open the onboarding tutorial overlay.
//!
//! Purely opt-in: this command (also listed in the command palette) is the
//! only way the tutorial opens — it never auto-shows.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
use crate::slash::{ModeSupport, Remedy};

/// Open the onboarding tutorial.
pub struct TutorialCommand;

impl SlashCommand for TutorialCommand {
    fn name(&self) -> &str {
        "tutorial"
    }

    fn aliases(&self) -> &[&str] {
        &["tour", "onboarding"]
    }

    fn description(&self) -> &str {
        "Quick tips to get the most out of EchoAgent Build"
    }

    fn usage(&self) -> &str {
        "/tutorial"
    }

    /// Gated off rather than merely hidden: minimal has no modal host, so the
    /// overlay's input intercept would freeze the session invisibly.
    fn mode_support(&self) -> ModeSupport {
        ModeSupport::FullscreenOnly(Remedy::SwitchMode {
            why: "the tutorial overlay needs fullscreen",
        })
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenTutorial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

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
    fn dispatches_open_tutorial() {
        let models = ModelState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Fullscreen,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        };
        assert!(matches!(
            TutorialCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::OpenTutorial)
        ));
    }
}
