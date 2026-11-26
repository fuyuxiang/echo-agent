//! `/rename` (alias `/title`) -- rename the current session.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};
use echo_agent_runtime::session::persistence::{MAX_TITLE_SCALARS, sanitize_rename_title};

/// Rename the current session's title/summary.
pub struct RenameCommand;

impl SlashCommand for RenameCommand {
    fn name(&self) -> &str {
        "rename"
    }

    fn aliases(&self) -> &[&str] {
        &["title"]
    }

    fn description(&self) -> &str {
        "Rename the current session"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/rename <title> | --auto"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<title>")
    }

    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        // Empty-args-only: a ghost row that survives typed input (including
        // `--auto`) steals Enter (`accept_slash_completion` replaces the
        // args range).
        if !args_query.trim().is_empty() {
            return None;
        }
        // Prefill is `rename_source_title`, not `entry_title` — see that
        // helper. `current_title` is already sanitized at sync time.
        let title = ctx.current_title?.trim();
        if title.is_empty() || title == "--auto" {
            return None;
        }
        Some(vec![ArgItem {
            display: title.to_owned(),
            match_text: title.to_owned(),
            insert_text: title.to_owned(),
            description: "current title".to_string(),
        }])
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error("No active session".to_string());
        }

        // Strip so `/rename --auto<BEL>` is still the reserved verb, not a
        // literal title. Also check the raw trim so `--auto\tmore` keeps
        // the trailing-text error (tab is a control and would otherwise
        // concatenate). `--automatic` stays a normal rename.
        let title = sanitize_rename_title(args);
        if is_auto_verb(&title) || is_auto_verb(args) {
            return CommandResult::Action(Action::ResetSessionTitleToAuto);
        }
        if auto_verb_has_trailing_text(args) || auto_verb_has_trailing_text(&title) {
            return CommandResult::Error("--auto takes no title".to_string());
        }

        if title.is_empty() {
            return CommandResult::Error("Usage: /rename <new title> | --auto".to_string());
        }
        if title.chars().count() > MAX_TITLE_SCALARS {
            return CommandResult::Error(format!(
                "title too long (max {MAX_TITLE_SCALARS} characters)"
            ));
        }

        CommandResult::Action(Action::RenameSession {
            title: title.into_owned(),
        })
    }
}

fn is_auto_verb(args: &str) -> bool {
    args.trim() == "--auto"
}

fn auto_verb_has_trailing_text(args: &str) -> bool {
    args.trim()
        .strip_prefix("--auto")
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::slash::command::AppCtx;

    fn app_ctx<'a>(models: &'a ModelState, current_title: Option<&'a str>) -> AppCtx<'a> {
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
            current_title,
        }
    }

    static EMPTY_BUNDLE: crate::app::bundle::BundleState = crate::app::bundle::BundleState {
        has_cache: false,
        version: String::new(),
        personas: Vec::new(),
        roles: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
        persona_details: Vec::new(),
        role_details: Vec::new(),
    };

    fn exec_ctx<'a>(
        models: &'a ModelState,
        session_id: Option<&'a agent_client_protocol::SessionId>,
    ) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id,
            bundle_state: &EMPTY_BUNDLE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn suggest_args_prefills_current_title() {
        let models = ModelState::default();
        let ctx = app_ctx(&models, Some("Fix Login Bug"));
        let items = RenameCommand
            .suggest_args(&ctx, "")
            .expect("should ghost-prefill");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text, "Fix Login Bug");
        assert_eq!(items[0].display, "Fix Login Bug");
    }

    #[test]
    fn suggest_args_via_slash_controller() {
        use crate::slash::{CommandRegistry, SlashController, SlashState};
        use std::sync::Arc;

        let mut ctrl = SlashController::new(
            CommandRegistry::new(vec![Arc::new(RenameCommand)]),
            std::path::PathBuf::from("."),
        );
        ctrl.set_current_title(Some("Fix Login Bug".into()));
        let state = SlashState::default();
        let models = ModelState::default();
        ctrl.refresh(&state, "/rename ", 8, &models);
        let snap = state.snapshot();
        assert_eq!(snap.matches.len(), 1);
        assert_eq!(snap.matches[0].insert_text, "Fix Login Bug");

        // Title change alone does not rebuild the snapshot (render used to
        // stop at set_current_title). A refresh after the update is what
        // offers the new ghost while `/rename ` is already open.
        ctrl.set_current_title(Some("Late Title".into()));
        assert_eq!(state.snapshot().matches[0].insert_text, "Fix Login Bug");
        ctrl.refresh(&state, "/rename ", 8, &models);
        assert_eq!(state.snapshot().matches[0].insert_text, "Late Title");

        ctrl.set_current_title(Some("   ".into()));
        assert!(ctrl.current_title().is_none());
    }

    #[test]
    fn suggest_args_none_without_title() {
        let models = ModelState::default();
        let ctx = app_ctx(&models, None);
        assert!(RenameCommand.suggest_args(&ctx, "").is_none());
        let ctx = app_ctx(&models, Some("   "));
        assert!(RenameCommand.suggest_args(&ctx, "").is_none());
    }

    #[test]
    fn suggest_args_none_once_user_has_typed() {
        let models = ModelState::default();
        let ctx = app_ctx(&models, Some("Fix Login Bug"));
        assert!(
            RenameCommand.suggest_args(&ctx, "Fix").is_none(),
            "typed prefix must not keep the ghost prefill (Enter would overwrite)"
        );
        assert!(RenameCommand.suggest_args(&ctx, "bug").is_none());
        assert!(RenameCommand.suggest_args(&ctx, "fl").is_none());
        // Whitespace-only query still counts as empty-args prefill.
        let items = RenameCommand
            .suggest_args(&ctx, "  ")
            .expect("whitespace-only query is still empty-args");
        assert_eq!(items[0].insert_text, "Fix Login Bug");
    }

    #[test]
    fn run_rejects_overlong_title() {
        let models = ModelState::default();
        let sid = agent_client_protocol::SessionId::new("s");
        let mut ctx = exec_ctx(&models, Some(&sid));
        let too_long: String = "é".repeat(MAX_TITLE_SCALARS + 1);
        match RenameCommand.run(&mut ctx, &too_long) {
            CommandResult::Error(msg) => {
                assert!(
                    msg.contains("title too long"),
                    "expected length error, got {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let ok: String = "é".repeat(MAX_TITLE_SCALARS);
        match RenameCommand.run(&mut ctx, &ok) {
            CommandResult::Action(Action::RenameSession { title }) => {
                assert_eq!(title, ok);
            }
            other => panic!("expected Action, got {other:?}"),
        }

        let padded = format!("  {}  ", "é".repeat(MAX_TITLE_SCALARS));
        match RenameCommand.run(&mut ctx, &padded) {
            CommandResult::Action(Action::RenameSession { title }) => {
                assert_eq!(title, ok);
            }
            other => panic!("padded 100 must accept, got {other:?}"),
        }

        let padded_over = format!("  {}  ", "é".repeat(MAX_TITLE_SCALARS + 1));
        assert!(matches!(
            RenameCommand.run(&mut ctx, &padded_over),
            CommandResult::Error(_)
        ));

        let esc_ok = format!("\u{1b}{}", "👍".repeat(MAX_TITLE_SCALARS));
        match RenameCommand.run(&mut ctx, &esc_ok) {
            CommandResult::Action(Action::RenameSession { title }) => {
                assert_eq!(title, "👍".repeat(MAX_TITLE_SCALARS));
            }
            other => panic!("ESC+100 thumbs must accept after strip, got {other:?}"),
        }
    }

    #[test]
    fn run_requires_session_and_nonblank_title() {
        let models = ModelState::default();
        let mut ctx = exec_ctx(&models, None);
        assert!(matches!(
            RenameCommand.run(&mut ctx, "Title"),
            CommandResult::Error(msg) if msg.contains("No active session")
        ));
        let sid = agent_client_protocol::SessionId::new("s");
        let mut ctx = exec_ctx(&models, Some(&sid));
        assert!(matches!(
            RenameCommand.run(&mut ctx, "   "),
            CommandResult::Error(_)
        ));
    }

    #[test]
    fn run_auto_is_reserved_sole_argument() {
        let models = ModelState::default();
        let sid = agent_client_protocol::SessionId::new("s");
        let mut ctx = exec_ctx(&models, Some(&sid));

        assert!(matches!(
            RenameCommand.run(&mut ctx, "--auto"),
            CommandResult::Action(Action::ResetSessionTitleToAuto)
        ));
        assert!(matches!(
            RenameCommand.run(&mut ctx, "  --auto  "),
            CommandResult::Action(Action::ResetSessionTitleToAuto)
        ));
        assert!(
            matches!(
                RenameCommand.run(&mut ctx, "--auto\u{07}"),
                CommandResult::Action(Action::ResetSessionTitleToAuto)
            ),
            "/rename --auto<BEL> must still be the reserved verb"
        );

        match RenameCommand.run(&mut ctx, "--auto Something") {
            CommandResult::Error(msg) => assert!(
                msg.contains("--auto takes no title"),
                "expected sole-argument error, got {msg}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
        match RenameCommand.run(&mut ctx, "--auto\tmore") {
            CommandResult::Error(msg) => assert!(
                msg.contains("--auto takes no title"),
                "tab-separated trailing text must error, got {msg}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }

        match RenameCommand.run(&mut ctx, "--automatic") {
            CommandResult::Action(Action::RenameSession { title }) => {
                assert_eq!(title, "--automatic");
            }
            other => panic!("--automatic must be a normal rename, got {other:?}"),
        }
        match RenameCommand.run(&mut ctx, "--auto-foo") {
            CommandResult::Action(Action::RenameSession { title }) => {
                assert_eq!(title, "--auto-foo");
            }
            other => panic!("--auto-foo must be a normal rename, got {other:?}"),
        }
        match RenameCommand.run(&mut ctx, "--autoSomething") {
            CommandResult::Action(Action::RenameSession { title }) => {
                assert_eq!(title, "--autoSomething");
            }
            other => panic!("glued --auto prefix must rename, got {other:?}"),
        }
        match RenameCommand.run(&mut ctx, "--AUTO") {
            CommandResult::Action(Action::RenameSession { title }) => {
                assert_eq!(title, "--AUTO");
            }
            other => panic!("--AUTO must be a normal rename, got {other:?}"),
        }
        match RenameCommand.run(&mut ctx, "  --auto Something") {
            CommandResult::Error(msg) => assert!(
                msg.contains("--auto takes no title"),
                "trimmed trailing text must error, got {msg}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }

        let mut no_session = exec_ctx(&models, None);
        assert!(matches!(
            RenameCommand.run(&mut no_session, "--auto"),
            CommandResult::Error(msg) if msg.contains("No active session")
        ));
    }

    #[test]
    fn suggest_args_skips_prefill_when_typing_auto_verb() {
        let models = ModelState::default();
        let ctx = app_ctx(&models, Some("Fix Login Bug"));
        assert!(
            RenameCommand.suggest_args(&ctx, "--auto").is_none(),
            "ghost-prefill must not attach a title after --auto"
        );
        assert!(RenameCommand.suggest_args(&ctx, "  --auto  ").is_none());
        assert!(RenameCommand.suggest_args(&ctx, "--auto ").is_none());
        assert!(
            RenameCommand.suggest_args(&ctx, "--auto\tmore").is_none(),
            "tab-separated --auto must not ghost-prefill"
        );

        let items = RenameCommand
            .suggest_args(&ctx, "")
            .expect("empty query still prefills");
        assert_eq!(items[0].insert_text, "Fix Login Bug");

        assert!(
            RenameCommand.suggest_args(&ctx, "Fix").is_none(),
            "typed title prefix must not keep the ghost prefill (Enter would overwrite)"
        );

        let reserved = app_ctx(&models, Some("--auto"));
        assert!(
            RenameCommand.suggest_args(&reserved, "").is_none(),
            "must not ghost-prefill the reserved literal as a title"
        );
    }
}
