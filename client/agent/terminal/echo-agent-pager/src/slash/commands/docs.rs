//! `/docs` -- open How-to Guides (in-TUI) or online Build docs.
//!
//! Bare `/docs` opens the same DocPicker as command-palette "How-to Guides".
//! `/docs web` opens https://docs.echo-agent.invalid/build/overview in the browser.
//! `/docs <title>` opens a single guide by title (case-insensitive).

use crate::app::actions::Action;
use crate::docs::{all_titles, find_doc};
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

/// Online Build docs landing page (hardcoded like other TUI deep-links; docs.echo-agent.invalid can redirect if the path moves).
pub const BUILD_DOCS_URL: &str = "https://docs.echo-agent.invalid/build/overview";

/// Open How-to Guides or online Build docs.
pub struct DocsCommand;

impl SlashCommand for DocsCommand {
    fn name(&self) -> &str {
        "docs"
    }

    fn aliases(&self) -> &[&str] {
        &["howto", "guides"]
    }

    fn description(&self) -> &str {
        "Open How-to Guides or online Build docs"
    }

    fn usage(&self) -> &str {
        "/docs [web|title]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("[web|title]")
    }

    fn suggest_args(&self, _ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        let mut items = vec![
            ArgItem {
                display: "how-to".into(),
                match_text: "how-to".into(),
                insert_text: "how-to".into(),
                description: "Browse in-TUI How-to Guides".into(),
            },
            ArgItem {
                display: "web".into(),
                match_text: "web".into(),
                insert_text: "web".into(),
                description: "Open docs.echo-agent.invalid/build in the browser".into(),
            },
        ];
        items.extend(all_titles().map(|title| ArgItem {
            display: title.into(),
            match_text: title.into(),
            insert_text: title.into(),
            description: format!("Open \"{title}\""),
        }));
        Some(items)
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if trimmed.is_empty() || is_howto_list_arg(trimmed) {
            return CommandResult::Action(Action::OpenHowtoGuides);
        }
        if is_web_arg(trimmed) {
            return CommandResult::Action(Action::OpenUrl(BUILD_DOCS_URL.into()));
        }
        match find_doc(trimmed) {
            Some(doc) => CommandResult::Action(Action::ShowReleaseNotes {
                title: doc.title.into(),
                content: doc.content.into(),
            }),
            None => CommandResult::Error(format!(
                "Unknown docs target {trimmed:?}. Try /docs, /docs web, or a guide title (e.g. /docs Getting Started)."
            )),
        }
    }
}

fn is_howto_list_arg(arg: &str) -> bool {
    matches!(
        arg.to_ascii_lowercase().as_str(),
        "how-to" | "howto" | "guides" | "guide" | "list" | "tui"
    )
}

fn is_web_arg(arg: &str) -> bool {
    matches!(
        arg.to_ascii_lowercase().as_str(),
        "web" | "online" | "browser" | "site" | "www"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;

    static DEFAULT_BUNDLE_STATE: crate::app::bundle::BundleState =
        crate::app::bundle::BundleState {
            has_cache: false,
            version: String::new(),
            personas: Vec::new(),
            roles: Vec::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            persona_details: Vec::new(),
            role_details: Vec::new(),
        };

    fn make_ctx<'a>(models: &'a ModelState) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot {
                multiline_mode: false,
                yolo_mode: false,
                ..crate::settings::PagerLocalSnapshot::default()
            },
        }
    }

    #[test]
    fn bare_docs_opens_howto_guides() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        assert!(matches!(
            DocsCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::OpenHowtoGuides)
        ));
    }

    #[test]
    fn howto_aliases_open_list() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        for args in ["how-to", "howto", "guides", "list", "tui"] {
            assert!(
                matches!(
                    DocsCommand.run(&mut ctx, args),
                    CommandResult::Action(Action::OpenHowtoGuides)
                ),
                "args={args:?}"
            );
        }
    }

    #[test]
    fn web_opens_build_docs_url() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        for args in ["web", "online", "browser"] {
            match DocsCommand.run(&mut ctx, args) {
                CommandResult::Action(Action::OpenUrl(url)) => {
                    assert_eq!(url, BUILD_DOCS_URL, "args={args:?}");
                }
                other => panic!("expected OpenUrl for args={args:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn title_opens_guide() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        match DocsCommand.run(&mut ctx, "Getting Started") {
            CommandResult::Action(Action::ShowReleaseNotes { title, content }) => {
                assert_eq!(title, "Getting Started");
                assert!(!content.is_empty());
            }
            other => panic!("expected ShowReleaseNotes, got {other:?}"),
        }
    }

    #[test]
    fn unknown_target_errors() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        assert!(matches!(
            DocsCommand.run(&mut ctx, "not-a-real-guide"),
            CommandResult::Error(_)
        ));
    }

    #[test]
    fn aliases_and_metadata() {
        let cmd = DocsCommand;
        assert_eq!(cmd.name(), "docs");
        assert_eq!(cmd.aliases(), &["howto", "guides"]);
        assert!(cmd.takes_args());
        assert!(!cmd.args_required());
    }

    #[test]
    fn suggest_args_includes_web_and_titles() {
        let models = ModelState::default();
        let cwd = std::path::Path::new(".");
        let ctx = AppCtx {
            models: &models,
            cwd,
            has_session_announcements: false,
            billing_surface_visible: true,
            usage_command_visible: true,
            workflows_available: true,
            saved_workflows: &[],
            workflow_runs: &[],
            screen_mode: crate::app::ScreenMode::Fullscreen,
            current_title: None,
        };
        let items = DocsCommand.suggest_args(&ctx, "").expect("suggestions");
        assert!(items.iter().any(|i| i.insert_text == "web"));
        assert!(items.iter().any(|i| i.insert_text == "how-to"));
        assert!(items.iter().any(|i| i.insert_text == "Getting Started"));
    }
}
