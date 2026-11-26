//! `/import-claude` -- open the interactive Claude settings import modal.
//!
//! This is the in-session entry point. The slash command dispatches the
//! shared `Action::ImportClaudeSettings` action; the dispatch handler
//! scans `.claude/settings*.json`, `~/.claude.json`, and `.mcp.json`,
//! populates the modal state, and the agent view overlays the modal on
//! top of the active session. The user gets the same selection UI as the
//! welcome screen's Ctrl-I shortcut.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Open the interactive Claude settings import modal in the active session.
pub struct ImportClaudeCommand;

impl SlashCommand for ImportClaudeCommand {
    fn name(&self) -> &str {
        "import-claude"
    }

    fn description(&self) -> &str {
        "Open the Claude settings import modal"
    }

    fn usage(&self) -> &str {
        "/import-claude"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if !trimmed.is_empty() {
            tracing::warn!("/import-claude does not accept arguments; ignoring");
        }
        CommandResult::Action(Action::ImportClaudeSettings)
    }
}
