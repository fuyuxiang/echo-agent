//! `/announcements` -- show or hide the announcement banner.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

const USAGE: &str = "Usage: /announcements hide | show";

/// Control the announcement banner (hide/show).
pub struct AnnouncementsCommand;

impl SlashCommand for AnnouncementsCommand {
    fn name(&self) -> &str {
        "announcements"
    }

    fn description(&self) -> &str {
        "Show or hide announcements"
    }

    fn usage(&self) -> &str {
        "/announcements hide | show"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("hide|show")
    }

    fn suggest_args(&self, _ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        Some(vec![
            ArgItem {
                display: "hide".to_string(),
                match_text: "hide".to_string(),
                insert_text: "hide".to_string(),
                description: "Hide the announcement banner".to_string(),
            },
            ArgItem {
                display: "show".to_string(),
                match_text: "show".to_string(),
                insert_text: "show".to_string(),
                description: "Show the announcement banner".to_string(),
            },
        ])
    }

    fn visible(&self, ctx: &AppCtx) -> bool {
        ctx.has_session_announcements
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        match args.split_whitespace().next().unwrap_or("") {
            "hide" => CommandResult::Action(Action::AnnouncementsHide),
            "show" => CommandResult::Action(Action::AnnouncementsShow),
            _ => CommandResult::Error(USAGE.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::slash::command::CommandExecCtx;

    fn run(args: &str) -> CommandResult {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        };
        AnnouncementsCommand.run(&mut ctx, args)
    }

    #[test]
    fn run_subcommands() {
        assert!(matches!(
            run("hide"),
            CommandResult::Action(Action::AnnouncementsHide)
        ));
        assert!(matches!(
            run("show"),
            CommandResult::Action(Action::AnnouncementsShow)
        ));
        assert!(matches!(
            run("  hide  "),
            CommandResult::Action(Action::AnnouncementsHide)
        ));
        assert!(matches!(
            run("hide extra"),
            CommandResult::Action(Action::AnnouncementsHide)
        ));
    }

    #[test]
    fn run_invalid_or_empty_shows_usage() {
        for args in ["", "foo", "next", "prev"] {
            match run(args) {
                CommandResult::Error(msg) => assert!(msg.contains("/announcements")),
                other => panic!("expected Error for {args:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn suggest_args_lists_subcommands() {
        let models = ModelState::default();
        let ctx = AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: true,
            billing_surface_visible: true,
            usage_command_visible: true,
            workflows_available: true,
            saved_workflows: &[],
            workflow_runs: &[],
            screen_mode: crate::app::ScreenMode::Fullscreen,
            current_title: None,
        };
        let items = AnnouncementsCommand
            .suggest_args(&ctx, "")
            .expect("suggestions");
        let names: Vec<_> = items.iter().map(|i| i.insert_text.as_str()).collect();
        assert_eq!(names, ["hide", "show"]);
    }

    #[test]
    fn visible_only_with_session_announcements() {
        let models = ModelState::default();
        let cmd = AnnouncementsCommand;
        // Flag is independent of the per-ID hidden set — true means menu
        // still offers /announcements after hide (so show remains discoverable).
        assert!(!cmd.visible(&AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            billing_surface_visible: true,
            usage_command_visible: true,
            workflows_available: true,
            saved_workflows: &[],
            workflow_runs: &[],
            screen_mode: crate::app::ScreenMode::Fullscreen,
            current_title: None,
        }));
        assert!(cmd.visible(&AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: true,
            billing_surface_visible: true,
            usage_command_visible: true,
            workflows_available: true,
            saved_workflows: &[],
            workflow_runs: &[],
            screen_mode: crate::app::ScreenMode::Fullscreen,
            current_title: None,
        }));
    }

    #[test]
    fn metadata() {
        let cmd = AnnouncementsCommand;
        assert_eq!(cmd.name(), "announcements");
        assert!(cmd.takes_args());
        assert!(cmd.args_required());
        assert_eq!(cmd.arg_placeholder(), Some("hide|show"));
    }
}
