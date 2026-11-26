//! `/history` -- open the prompt-history search overlay.
//!
//! Search mode over the panel Up-arrow browsing uses: fuzzy-search the
//! session's prior prompts, Enter/Tab drops the selection back into the
//! composer. The slash pipeline clears the composer before dispatch, so
//! the overlay opens with an empty query over the full history.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Open the prompt-history search overlay via `/history`.
pub struct HistoryCommand;

impl SlashCommand for HistoryCommand {
    fn name(&self) -> &str {
        "history"
    }

    fn description(&self) -> &str {
        "Search prompt history"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/history"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::OpenHistorySearch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

    fn make_ctx<'a>(models: &'a ModelState, bundle: &'a BundleState) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn run_dispatches_open_history_search() {
        let cmd = HistoryCommand;
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = make_ctx(&models, &bundle);
        let result = cmd.run(&mut ctx, "");
        assert!(matches!(
            result,
            CommandResult::Action(Action::OpenHistorySearch)
        ));
    }

    /// `/history` resolves via the real builtin registry (guards against a
    /// name collision silently dropping it).
    #[test]
    fn resolves_via_builtin_registry() {
        let reg = crate::slash::registry::CommandRegistry::new(
            crate::slash::commands::builtin_commands(),
        );
        let resolved = reg
            .get("history")
            .expect("/history must resolve to a command");
        assert_eq!(resolved.name(), "history");
    }
}
