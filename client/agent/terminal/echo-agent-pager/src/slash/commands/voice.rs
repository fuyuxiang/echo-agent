//! `/voice` — toggle dictation: start recording now, stop with Esc or Enter
//! (Enter also sends). Not written to `config.toml`.
//!
//! The keyboard chord is **Ctrl+Space** or **F8** (both work; F8 is a fallback
//! where Ctrl+Space is taken — e.g. macOS input switching). Its behavior follows
//! `[ui].voice_capture_mode`: `toggle` (press starts, press again stops — like
//! `/voice`) or `hold`-to-talk (hold to record, release to stop), with `hold`
//! available only on terminals that report key releases (Kitty protocol) and
//! falling back to toggle elsewhere. The recording banner is the only feedback;
//! no toast.
//!
//! Dictation works on the agent screen (into the prompt) and on the dashboard
//! (into the dispatch / new-agent input).
//!
//! **Scope.** On for the rest of the process; re-open `echoagent` for a clean slate.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Session voice mode enable via `/voice`.
pub struct VoiceCommand;

impl SlashCommand for VoiceCommand {
    fn name(&self) -> &str {
        "voice"
    }

    fn description(&self) -> &str {
        // Chord is Ctrl+Space or F8. Without key releases hold-to-talk is
        // impossible, so it's always toggle — say so. With them it's
        // configurable (toggle or hold via `voice_capture_mode`), so leave the
        // behavior unspecified.
        if crate::app::kitty_releases_reported() {
            "Dictation (Ctrl+Space/F8; Esc/Enter to stop)"
        } else {
            "Toggle dictation (Ctrl+Space/F8; Esc/Enter to stop)"
        }
    }

    fn usage(&self) -> &str {
        "/voice"
    }

    /// Dictation targets a prompt box: the agent prompt in a live session, or
    /// the dashboard's dispatch (new-agent) input. Session-scoped (no effect on
    /// the welcome screen) but still offered on the dashboard.
    fn session_scoped(&self) -> bool {
        true
    }

    fn offered_when_session_less(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        // Toggle, mirroring the voice key: starts dictation, or stops it
        // if already recording (Esc/Enter also stop).
        CommandResult::Action(Action::VoiceToggle)
    }
}
