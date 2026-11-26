//! `/scroll-debug` — toggle the scroll-diagnostics HUD
//! ([`crate::views::scroll_debug_hud`]).
//!
//! Hidden diagnostic (the `/gboom` pattern): typeable but never listed in
//! the dropdown, and any argument passes through like an unknown command.
//! Pairs with `ECHO_AGENT_SCROLL_DEBUG=1`, which enables the HUD from startup;
//! this command flips it live mid-session.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, CommandExecCtx, CommandResult, SlashCommand};

/// Hidden toggle for the scroll-debug HUD.
pub struct ScrollDebugCommand;

impl SlashCommand for ScrollDebugCommand {
    fn name(&self) -> &str {
        "scroll-debug"
    }

    fn description(&self) -> &str {
        // Never shown: the command is hidden from the dropdown.
        "Toggle the scroll-diagnostics HUD"
    }

    fn usage(&self) -> &str {
        "/scroll-debug"
    }

    /// Diagnostic: typeable, never listed.
    fn visible(&self, _ctx: &AppCtx) -> bool {
        false
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if !args.trim().is_empty() {
            // With arguments, behave as if the command didn't exist.
            return CommandResult::PassThrough(format!("/scroll-debug {args}"));
        }
        CommandResult::Action(Action::ToggleScrollDebugHud)
    }
}
