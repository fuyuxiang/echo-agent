//! `/expand` -- re-print the last collapsed block, fully expanded (minimal mode).
//!
//! In the scrollback-native minimal mode (`echoagent --minimal`) finalized blocks are
//! printed once into the terminal's native scrollback, with reasoning collapsed
//! and large tool output truncated (design decision K9). Committed terminal text
//! can't be mutated, so "expanding" one is an honest re-print of the same block
//! in full below the conversation (K10). `/expand` is the slash-command twin of
//! the `Ctrl+E` chord; both walk backwards through the most-recently committed
//! folded blocks.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
use crate::slash::{ModeSupport, Remedy};

/// Re-print the last collapsed/truncated block, fully expanded (minimal mode).
pub struct ExpandCommand;

impl SlashCommand for ExpandCommand {
    fn name(&self) -> &str {
        "expand"
    }

    fn description(&self) -> &str {
        "Re-print the last collapsed block, fully expanded (minimal mode)"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/expand"
    }

    fn mode_support(&self) -> ModeSupport {
        ModeSupport::MinimalOnly(Remedy::UseInstead(
            "press Tab to focus the scrollback, then → on the block",
        ))
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error("No active session".to_string());
        }
        CommandResult::Action(Action::MinimalExpandLast)
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

    fn ctx<'a>(
        models: &'a ModelState,
        session_id: Option<&'a agent_client_protocol::SessionId>,
        screen_mode: crate::app::ScreenMode,
    ) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            billing_surface_visible: true,
            usage_command_visible: true,
            screen_mode,
            pager_state: PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn minimal_with_session_dispatches_expand_action() {
        let models = ModelState::default();
        let sid = agent_client_protocol::SessionId::from("s1".to_string());
        let mut c = ctx(&models, Some(&sid), crate::app::ScreenMode::Minimal);
        assert!(matches!(
            ExpandCommand.run(&mut c, ""),
            CommandResult::Action(Action::MinimalExpandLast)
        ));
    }

    #[test]
    fn minimal_without_session_errors() {
        let models = ModelState::default();
        let mut c = ctx(&models, None, crate::app::ScreenMode::Minimal);
        match ExpandCommand.run(&mut c, "") {
            CommandResult::Error(msg) => assert!(msg.contains("No active session")),
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
