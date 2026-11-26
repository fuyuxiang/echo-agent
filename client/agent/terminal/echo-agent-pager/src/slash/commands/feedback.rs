//! `/feedback`: send session feedback.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Full TUI: always opens the report card (`/feedback <text>` prefills it).
/// Minimal: bare opens the pane; `/feedback <text>` still submits immediately.
pub struct FeedbackCommand;

impl SlashCommand for FeedbackCommand {
    fn name(&self) -> &str {
        "feedback"
    }

    fn description(&self) -> &str {
        "Send feedback about the current session"
    }

    fn usage(&self) -> &str {
        "/feedback [text]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("[feedback text]")
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        // Composer images ride the action, but dispatch attaches them (the
        // command layer never sees the prompt), so they start empty here.
        let result = if ctx.screen_mode.is_minimal() {
            if trimmed.is_empty() {
                CommandResult::Action(Action::OpenFeedbackPane {
                    prefill: None,
                    images: Default::default(),
                })
            } else {
                CommandResult::Action(Action::SendFeedback {
                    text: trimmed.to_string(),
                    images: Default::default(),
                    // Minimal mode never shows the trace-consent card.
                    trace: None,
                })
            }
        } else {
            CommandResult::Action(Action::OpenFeedbackPane {
                prefill: (!trimmed.is_empty()).then(|| trimmed.to_string()),
                images: Default::default(),
            })
        };
        let action = match &result {
            CommandResult::Action(Action::OpenFeedbackPane { prefill, .. }) => {
                if prefill.is_some() {
                    "open_prefill"
                } else {
                    "open_empty"
                }
            }
            CommandResult::Action(Action::SendFeedback { .. }) => "send_immediate",
            _ => "other",
        };
        crate::unified_log::info(
            "feedback.command",
            ctx.session_id.map(|s| s.0.as_ref()),
            Some(serde_json::json!({
                "screen_mode": ctx.screen_mode.meta_label(),
                "arg_chars": trimmed.chars().count(),
                "action": action,
            })),
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;

    fn make_ctx(models: &ModelState) -> CommandExecCtx<'_> {
        let bundle = Box::leak(Box::new(crate::app::bundle::BundleState::default()));
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    /// The whitespace case matters: the composer keeps a trailing space while the user is still typing the command.
    #[test]
    fn full_tui_always_opens_the_pane_and_prefills_inline_text() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        let cmd = FeedbackCommand;

        for args in ["", "   ", "\t"] {
            match cmd.run(&mut ctx, args) {
                CommandResult::Action(Action::OpenFeedbackPane {
                    prefill: None,
                    images,
                }) => {
                    assert!(images.is_empty(), "images attach at dispatch, not here");
                }
                other => panic!("{args:?} should open the pane, got {other:?}"),
            }
        }

        match cmd.run(&mut ctx, "  the tool crashed  ") {
            CommandResult::Action(Action::OpenFeedbackPane {
                prefill: Some(text),
                images,
            }) => {
                assert_eq!(
                    text, "the tool crashed",
                    "inline text is trimmed into the prefill"
                );
                assert!(images.is_empty(), "images attach at dispatch, not here");
            }
            other => panic!("full TUI inline text should prefill the pane, got {other:?}"),
        }
    }

    #[test]
    fn minimal_inline_text_still_submits() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        ctx.screen_mode = crate::app::ScreenMode::Minimal;
        let cmd = FeedbackCommand;

        match cmd.run(&mut ctx, "") {
            CommandResult::Action(Action::OpenFeedbackPane { prefill: None, .. }) => {}
            other => panic!("minimal bare should open the pane, got {other:?}"),
        }
        match cmd.run(&mut ctx, "  the tool crashed  ") {
            CommandResult::Action(Action::SendFeedback {
                text,
                images,
                trace: None,
            }) => {
                assert_eq!(
                    text, "the tool crashed",
                    "minimal inline text still submits"
                );
                assert!(images.is_empty(), "images attach at dispatch, not here");
            }
            other => panic!("minimal inline text should submit, got {other:?}"),
        }
    }
}
