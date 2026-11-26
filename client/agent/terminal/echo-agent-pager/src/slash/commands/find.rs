//! `/find` -- open an incremental search over the conversation scrollback.
//!
//! In simple mode a bare `/` goes to the prompt, so simple-mode users can't
//! reach the vim `/` scrollback search. `/find` focuses the scrollback pane
//! and opens the same search from either mode.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
use crate::slash::{ModeSupport, Remedy};

/// Open scrollback search via `/find`.
pub struct FindCommand;

impl SlashCommand for FindCommand {
    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Search the conversation scrollback"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/find [text]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("[text]")
    }

    fn mode_support(&self) -> ModeSupport {
        ModeSupport::FullscreenOnly(Remedy::SwitchMode {
            why: "minimal has no scrollback pane: use your terminal's own search",
        })
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        // whitespace-only args open a blank search.
        let initial = args.trim();
        let query = (!initial.is_empty()).then(|| initial.to_string());
        CommandResult::Action(Action::OpenScrollbackSearch(query))
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

    fn make_ctx(models: &ModelState) -> CommandExecCtx<'_> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn find_returns_open_scrollback_search_action() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        let cmd = FindCommand;
        assert!(matches!(
            cmd.run(&mut ctx, ""),
            CommandResult::Action(Action::OpenScrollbackSearch(None))
        ));
    }

    #[test]
    fn find_with_word_carries_it_as_initial_query() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        let cmd = FindCommand;
        let CommandResult::Action(Action::OpenScrollbackSearch(query)) = cmd.run(&mut ctx, "foo")
        else {
            panic!("/find foo must open scrollback search");
        };
        assert_eq!(query.as_deref(), Some("foo"));
    }

    #[test]
    fn find_with_blank_args_carries_no_initial_query() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        let cmd = FindCommand;
        // Both a bare `/find` and whitespace-only args open a blank search.
        for args in ["", "   "] {
            assert!(matches!(
                cmd.run(&mut ctx, args),
                CommandResult::Action(Action::OpenScrollbackSearch(None))
            ));
        }
    }

    #[test]
    fn find_advertises_optional_text_arg() {
        // Pins the slash-arg contract so completion-accept appends a trailing
        // space and the `[text]` placeholder shows while typing; bare `/find`
        // stays valid (args not required).
        let cmd = FindCommand;
        assert!(cmd.takes_args());
        assert!(!cmd.args_required());
        assert_eq!(cmd.arg_placeholder(), Some("[text]"));
    }
}
