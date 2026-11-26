//! `/minimal` and `/fullscreen` — session-scoped in-process screen-mode
//! switch, performed by the event loop via `app::mode_switch`.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
use crate::slash::{ModeSupport, Remedy};

/// Reopen the active session in the other screen mode (`/minimal` ⇄ `/fullscreen`).
pub struct ScreenModeSwitchCommand {
    /// `true` → `/minimal` (fullscreen → scrollback-native);
    /// `false` → `/fullscreen` (minimal → alt-screen TUI).
    to_minimal: bool,
}

impl ScreenModeSwitchCommand {
    /// `/minimal`: offered in the full TUI (alt-screen or `--no-alt-screen`
    /// inline), switches this session to scrollback-native rendering.
    pub const fn minimal() -> Self {
        Self { to_minimal: true }
    }

    /// `/fullscreen` (alias `/full`): offered in minimal, switches this
    /// session to the alt-screen TUI.
    pub const fn fullscreen() -> Self {
        Self { to_minimal: false }
    }

    fn target_label(&self) -> &'static str {
        if self.to_minimal {
            "minimal"
        } else {
            "fullscreen"
        }
    }
}

impl SlashCommand for ScreenModeSwitchCommand {
    fn name(&self) -> &str {
        self.target_label()
    }

    fn aliases(&self) -> &[&str] {
        if self.to_minimal { &[] } else { &["full"] }
    }

    fn description(&self) -> &str {
        if self.to_minimal {
            "Switch this session to minimal (scrollback-native) mode, back with /fullscreen"
        } else {
            "Switch this session to fullscreen mode, back with /minimal"
        }
    }

    fn usage(&self) -> &str {
        if self.to_minimal {
            "/minimal"
        } else {
            "/fullscreen"
        }
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn mode_support(&self) -> ModeSupport {
        if self.to_minimal {
            ModeSupport::FullscreenOnly(Remedy::AlreadyInMode)
        } else {
            ModeSupport::MinimalOnly(Remedy::AlreadyInMode)
        }
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error(format!(
                "No active session to reopen in {} mode",
                self.target_label(),
            ));
        }
        CommandResult::Action(Action::RelaunchInScreenMode {
            minimal: self.to_minimal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::ScreenMode;
    use crate::app::bundle::BundleState;

    fn exec_ctx<'a>(
        models: &'a ModelState,
        bundle: &'a BundleState,
        mode: ScreenMode,
        session: Option<&'a agent_client_protocol::SessionId>,
    ) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: session,
            bundle_state: bundle,
            screen_mode: mode,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn run_returns_relaunch_action_with_session() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let sid = agent_client_protocol::SessionId::from("sess-abc".to_string());

        for mode in [ScreenMode::Fullscreen, ScreenMode::Inline] {
            let mut ctx = exec_ctx(&models, &bundle, mode, Some(&sid));
            assert!(matches!(
                ScreenModeSwitchCommand::minimal().run(&mut ctx, ""),
                CommandResult::Action(Action::RelaunchInScreenMode { minimal: true })
            ));
        }

        let mut ctx = exec_ctx(&models, &bundle, ScreenMode::Minimal, Some(&sid));
        assert!(matches!(
            ScreenModeSwitchCommand::fullscreen().run(&mut ctx, ""),
            CommandResult::Action(Action::RelaunchInScreenMode { minimal: false })
        ));
    }

    #[test]
    fn run_errors_without_session() {
        let models = ModelState::default();
        let bundle = BundleState::default();

        let mut ctx = exec_ctx(&models, &bundle, ScreenMode::Fullscreen, None);
        assert!(matches!(
            ScreenModeSwitchCommand::minimal().run(&mut ctx, ""),
            CommandResult::Error(msg) if msg.contains("No active session")
        ));

        let mut ctx = exec_ctx(&models, &bundle, ScreenMode::Minimal, None);
        assert!(matches!(
            ScreenModeSwitchCommand::fullscreen().run(&mut ctx, ""),
            CommandResult::Error(msg) if msg.contains("No active session")
        ));
    }
}
