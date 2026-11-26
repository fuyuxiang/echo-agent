//! `/release-notes` -- view release notes for the current version.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Show release notes for the current pager version.
pub struct ReleaseNotesCommand;

impl SlashCommand for ReleaseNotesCommand {
    fn name(&self) -> &str {
        "release-notes"
    }

    fn aliases(&self) -> &[&str] {
        &["changelog"]
    }

    fn description(&self) -> &str {
        "View release notes for the current version"
    }

    fn usage(&self) -> &str {
        "/release-notes"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        let changelog = echo_agent_runtime::util::changelog::ChangelogManager::new().fetch();
        match changelog.markdown {
            Some(content) => CommandResult::Action(Action::ShowReleaseNotes {
                title: "Release Notes".to_string(),
                content: content.trim().to_string(),
            }),
            None => CommandResult::Error("No release notes available (offline).".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_notes_metadata() {
        let cmd = ReleaseNotesCommand;
        assert_eq!(cmd.name(), "release-notes");
        assert_eq!(cmd.aliases(), &["changelog"]);
        assert!(!cmd.takes_args());
    }

    #[test]
    fn release_notes_returns_action_or_error() {
        let models = crate::acp::model_state::ModelState::default();
        let mut ctx = super::super::tests::make_ctx(&models);
        let result = ReleaseNotesCommand.run(&mut ctx, "");
        assert!(
            matches!(result, CommandResult::Action(_) | CommandResult::Error(_)),
            "expected Action or Error, got {result:?}"
        );
    }
}
