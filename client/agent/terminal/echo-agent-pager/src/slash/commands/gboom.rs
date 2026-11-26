//! `/gboom` -- hidden easter egg. Launches a tiny raycaster shooter
//! rendered through the kitty graphics protocol.
//!
//! Never listed in the slash dropdown (`visible()` is false) but executes
//! when typed exactly as `/gboom`; with any argument it passes through to
//! the shell like an unknown command, so only the bare invocation triggers.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, CommandExecCtx, CommandResult, SlashCommand};

/// Hidden GBOOM easter egg.
pub struct GboomCommand;

impl SlashCommand for GboomCommand {
    fn name(&self) -> &str {
        "gboom"
    }

    fn description(&self) -> &str {
        // Never shown: the command is hidden from the dropdown.
        "Hidden easter egg"
    }

    fn usage(&self) -> &str {
        "/gboom"
    }

    /// Easter egg: typeable, never listed.
    fn visible(&self, _ctx: &AppCtx) -> bool {
        false
    }

    /// Needs an agent view to render in.
    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if !args.trim().is_empty() {
            // With arguments, behave as if the command didn't exist:
            // forward the text to the shell/model with the args untouched.
            return CommandResult::PassThrough(format!("/gboom {args}"));
        }
        CommandResult::Action(Action::OpenGboom)
    }
}
