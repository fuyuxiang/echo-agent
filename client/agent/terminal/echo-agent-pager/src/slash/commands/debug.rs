//! `/debug` — debug-overlay toggles (scroll HUD, FPS HUD, scroll log).
//!
//! Registration/visibility split: the command is registered on EVERY binary
//! and fully functional in release — like the hidden diagnostics it fronts
//! (`/scroll-debug`, `/gboom`) — but it is LISTED (dropdown, completion,
//! recognized-token highlight via `visible()`) only on debug binaries
//! (`cfg(debug_assertions)`). Discoverable where developers live, out of
//! sight for users, yet still typeable in the field when support asks.
//!
//! Subcommands (args-based; a popup menu can come later):
//! - `/debug` bare — print the toggles and their state to the transcript.
//! - `/debug scroll` — the scroll-diagnostics HUD; same
//!   [`Action::ToggleScrollDebugHud`] as `/scroll-debug`, which stays
//!   registered as the hidden long-form alias.
//! - `/debug fps` — the release-safe FPS HUD
//!   ([`crate::views::fps_hud`]).
//! - `/debug log` — the scroll flight recorder
//!   ([`crate::input::scroll_log`]), runtime-constructed to a fresh
//!   timestamped path.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

/// Whether `/debug` is listed on completion surfaces. `visible()` returns
/// this constant, so release invisibility is pinned by the constant's shape
/// (`cfg!(debug_assertions)`) rather than a runtime check — tests always
/// compile with `debug_assertions`, so the release half is untestable by
/// assertion and locked by construction instead.
pub const LISTED_IN_COMPLETIONS: bool = cfg!(debug_assertions);

/// Subcommand name/description pairs (single source for run + suggestions).
const SUBCOMMANDS: &[(&str, &str)] = &[
    ("scroll", "Toggle the scroll-diagnostics HUD"),
    ("fps", "Toggle the FPS overlay"),
    ("log", "Toggle the scroll flight recorder (JSONL)"),
];

/// Debug-overlay toggles; listed only on debug binaries.
pub struct DebugCommand;

impl SlashCommand for DebugCommand {
    fn name(&self) -> &str {
        "debug"
    }

    fn description(&self) -> &str {
        "Toggle debug overlays"
    }

    fn usage(&self) -> &str {
        "/debug [scroll|fps|log]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("scroll | fps | log")
    }

    /// Debug binaries only; release keeps it registered but unlisted.
    fn visible(&self, _ctx: &AppCtx) -> bool {
        LISTED_IN_COMPLETIONS
    }

    fn suggest_args(&self, _ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        Some(
            SUBCOMMANDS
                .iter()
                .map(|&(name, desc)| ArgItem {
                    display: name.to_string(),
                    match_text: name.to_string(),
                    insert_text: name.to_string(),
                    description: desc.to_string(),
                })
                .collect(),
        )
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        match args.trim() {
            // Bare: the cheapest useful menu — a status line in scrollback.
            "" => CommandResult::Action(Action::ShowDebugStatus),
            "scroll" => CommandResult::Action(Action::ToggleScrollDebugHud),
            "fps" => CommandResult::Action(Action::ToggleFpsHud),
            "log" => CommandResult::Action(Action::ToggleScrollLog),
            other => CommandResult::Error(format!(
                "Unknown /debug option '{other}'. Usage: /debug [scroll|fps|log]"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::slash::commands::scroll_debug::ScrollDebugCommand;
    use crate::slash::commands::tests::make_ctx;

    fn app_ctx(models: &ModelState) -> AppCtx<'_> {
        AppCtx {
            models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            billing_surface_visible: true,
            usage_command_visible: true,
            workflows_available: true,
            saved_workflows: &[],
            workflow_runs: &[],
            screen_mode: crate::app::ScreenMode::Fullscreen,
            current_title: None,
        }
    }

    /// Tests compile with `debug_assertions`, so this asserts the
    /// debug-binary half live: `/debug` must be visible here. The release
    /// half (invisible) is untestable from a debug test build and pinned by
    /// mechanism instead — `visible()` returns `LISTED_IN_COMPLETIONS =
    /// cfg!(debug_assertions)`, which a release compile evaluates to
    /// `false` by construction; the `assert_eq!` locks `visible()` to that
    /// constant under whichever profile compiles the test.
    #[test]
    fn debug_listed_on_debug_binaries_only() {
        let models = ModelState::default();
        let listed = DebugCommand.visible(&app_ctx(&models));
        assert_eq!(
            listed,
            cfg!(debug_assertions),
            "visible() must track the binary profile"
        );
        assert_eq!(listed, LISTED_IN_COMPLETIONS);
    }

    /// `/debug scroll` and `/scroll-debug` must stay routed to the SAME
    /// action — the HUD has one toggle, two spellings.
    #[test]
    fn debug_scroll_routes_to_same_action_as_scroll_debug() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        assert!(matches!(
            DebugCommand.run(&mut ctx, "scroll"),
            CommandResult::Action(Action::ToggleScrollDebugHud)
        ));
        assert!(matches!(
            ScrollDebugCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::ToggleScrollDebugHud)
        ));
    }

    #[test]
    fn debug_bare_emits_status() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        for args in ["", "   "] {
            assert!(matches!(
                DebugCommand.run(&mut ctx, args),
                CommandResult::Action(Action::ShowDebugStatus)
            ));
        }
    }

    #[test]
    fn debug_fps_and_log_route_to_their_toggles() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        assert!(matches!(
            DebugCommand.run(&mut ctx, " fps "),
            CommandResult::Action(Action::ToggleFpsHud)
        ));
        assert!(matches!(
            DebugCommand.run(&mut ctx, "log"),
            CommandResult::Action(Action::ToggleScrollLog)
        ));
    }

    #[test]
    fn debug_junk_subcommand_errors_helpfully() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        match DebugCommand.run(&mut ctx, "wat") {
            CommandResult::Error(msg) => {
                assert!(msg.contains("wat"), "must echo the bad option: {msg}");
                assert!(
                    msg.contains("scroll") && msg.contains("fps") && msg.contains("log"),
                    "must list the valid options: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn debug_suggest_args_lists_subcommands() {
        let models = ModelState::default();
        let items = DebugCommand
            .suggest_args(&app_ctx(&models), "")
            .expect("suggestions");
        let names: Vec<&str> = items.iter().map(|i| i.insert_text.as_str()).collect();
        assert_eq!(names, vec!["scroll", "fps", "log"]);
    }
}
