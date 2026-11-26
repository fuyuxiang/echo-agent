//! `/transcript` -- view the full conversation transcript in `$PAGER`.
//!
//! Renders the current session's transcript to a temp Markdown file and opens
//! it in the user's pager (default `less`), suspending the inline TUI until the
//! pager exits. Primarily for minimal mode, where there is no interactive
//! scrollback pane and older blocks have scrolled into native history — but it
//! works in every render mode.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// View the full conversation transcript in `$PAGER`.
pub struct TranscriptCommand;

impl SlashCommand for TranscriptCommand {
    fn name(&self) -> &str {
        "transcript"
    }

    fn aliases(&self) -> &[&str] {
        &["log"]
    }

    fn description(&self) -> &str {
        "View the conversation transcript in your pager ($PAGER)"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/transcript"
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error("No active session to view".to_string());
        }
        CommandResult::Action(Action::OpenTranscriptPager)
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
    fn no_session_errors() {
        let models = ModelState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Minimal,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        };
        match TranscriptCommand.run(&mut ctx, "") {
            CommandResult::Error(msg) => assert!(msg.contains("No active session")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn with_session_dispatches_open_transcript_pager() {
        let models = ModelState::default();
        let sid = agent_client_protocol::SessionId::from("s1".to_string());
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: Some(&sid),
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Minimal,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        };
        assert!(matches!(
            TranscriptCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::OpenTranscriptPager)
        ));
        // Args are ignored — same dispatch.
        assert!(matches!(
            TranscriptCommand.run(&mut ctx, "anything"),
            CommandResult::Action(Action::OpenTranscriptPager)
        ));
    }
}
