use agent_client_protocol as acp;
use echo_agent_tools::implementations::echo_agent_build::{
    LoopFireMode, SCHEDULER_CREATE_TOOL_NAME, loop_schedule_instruction, loop_usage_message,
};

use crate::slash::command::{CommandExecCtx, CommandResult, ScheduledTaskPreview, SlashCommand};

/// Pre-built slice for `LoopCommand::required_tools()`. Lifted to a
/// module-level constant so the trait method can return a `'static`
/// slice; the constant pulls the canonical name from `echo-agent-tools`
/// so a tool rename surfaces here at compile time.
const LOOP_REQUIRED_TOOLS: &[&str] = &[SCHEDULER_CREATE_TOOL_NAME];

pub struct LoopCommand;

/// Split `/loop` args into an optional leading compact interval token (only for
/// seeding the provisional preview) and the prompt. Returns `Some(token)` only
/// for a `^\d+[smhd]$` first token followed by prompt text; otherwise `None`,
/// leaving the model to derive the real interval. There is no host-side default.
fn parse_loop_args(args: &str) -> (Option<&str>, &str) {
    let trimmed = args.trim();
    if let Some(space) = trimmed.find(char::is_whitespace) {
        let first = &trimmed[..space];
        let rest = trimmed[space..].trim_start();
        if is_interval_token(first) && !rest.is_empty() {
            return (Some(first), rest);
        }
    }
    (None, trimmed)
}

/// Whether a token is a schedulable interval: non-zero digits followed by one
/// of s/m/h/d. Zero is rejected so the preview never shows a cadence the tool
/// would reject (`parse_interval` errors on zero).
fn is_interval_token(s: &str) -> bool {
    if s.len() < 2 {
        return false;
    }
    let (digits, suffix) = s.split_at(s.len() - 1);
    matches!(suffix, "s" | "m" | "h" | "d")
        && digits.chars().all(|c| c.is_ascii_digit())
        && digits.parse::<u64>().is_ok_and(|n| n > 0)
}

/// Convert an interval token like "5m" to a human string like "every 5 minutes".
fn interval_to_human(token: &str) -> String {
    let (digits, suffix) = token.split_at(token.len() - 1);
    let n: u64 = digits.parse().unwrap_or(0);
    match suffix {
        "s" => {
            if n <= 1 {
                "every 1 second".into()
            } else {
                format!("every {n} seconds")
            }
        }
        "m" => {
            if n == 1 {
                "every 1 minute".into()
            } else {
                format!("every {n} minutes")
            }
        }
        "h" => {
            if n == 1 {
                "every 1 hour".into()
            } else {
                format!("every {n} hours")
            }
        }
        "d" => {
            if n == 1 {
                "every 1 day".into()
            } else {
                format!("every {n} days")
            }
        }
        _ => format!("every {token}"),
    }
}

impl SlashCommand for LoopCommand {
    fn name(&self) -> &str {
        "loop"
    }

    fn description(&self) -> &str {
        "Run a prompt on a recurring interval"
    }

    fn usage(&self) -> &str {
        "/loop [interval] <prompt>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("[interval] <prompt>")
    }

    fn required_tools(&self) -> &[&str] {
        LOOP_REQUIRED_TOOLS
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if args.trim().is_empty() {
            return CommandResult::Message(loop_usage_message().to_string());
        }

        let (interval_token, prompt) = parse_loop_args(args);
        let fire_mode = if ctx.pager_state.scheduler_background_loops {
            LoopFireMode::Detached
        } else {
            LoopFireMode::InSession
        };

        // Show a concrete cadence only for an unambiguous leading token;
        // otherwise a neutral placeholder, since the authoritative schedule
        // arrives via the model's scheduler_create -> ScheduledTaskCreated and
        // replaces this provisional entry.
        let human_schedule = match interval_token {
            Some(token) => interval_to_human(token),
            None => "scheduling…".to_string(),
        };

        CommandResult::InjectSkill {
            display_text: format!("/loop {args}"),
            prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
                loop_schedule_instruction(args, fire_mode),
            ))],
            display_as_skill: false,
            scheduled_task_preview: Some(ScheduledTaskPreview {
                prompt: prompt.to_string(),
                human_schedule,
                next_fire_at: None,
                tag: "loop".into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::slash::command::CommandExecCtx;

    #[test]
    fn parse_with_explicit_interval() {
        let (interval, prompt) = parse_loop_args("5m check deploy status");
        assert_eq!(interval, Some("5m"));
        assert_eq!(prompt, "check deploy status");
    }

    #[test]
    fn parse_without_leading_interval_yields_none() {
        let (interval, prompt) = parse_loop_args("check deploy status");
        assert_eq!(interval, None);
        assert_eq!(prompt, "check deploy status");
    }

    #[test]
    fn parse_hours() {
        let (interval, prompt) = parse_loop_args("2h run tests");
        assert_eq!(interval, Some("2h"));
        assert_eq!(prompt, "run tests");
    }

    #[test]
    fn parse_days() {
        let (interval, prompt) = parse_loop_args("1d daily report");
        assert_eq!(interval, Some("1d"));
        assert_eq!(prompt, "daily report");
    }

    #[test]
    fn parse_seconds() {
        let (interval, prompt) = parse_loop_args("60s ping health");
        assert_eq!(interval, Some("60s"));
        assert_eq!(prompt, "ping health");
    }

    #[test]
    fn parse_interval_token_without_prompt_yields_none() {
        // A bare interval token with no prompt text is treated as the prompt;
        // there is no interval to extract for the preview.
        let (interval, prompt) = parse_loop_args("5m");
        assert_eq!(interval, None);
        assert_eq!(prompt, "5m");
    }

    #[test]
    fn parse_non_interval_first_token_yields_none() {
        let (interval, prompt) = parse_loop_args("check 5m deploy");
        assert_eq!(interval, None);
        assert_eq!(prompt, "check 5m deploy");
    }

    #[test]
    fn parse_empty_args_yields_none() {
        assert_eq!(parse_loop_args("   "), (None, ""));
        assert_eq!(parse_loop_args(""), (None, ""));
    }

    #[test]
    fn malformed_leading_tokens_yield_none() {
        // Exercises every rejecting branch of `is_interval_token` via
        // `parse_loop_args`: bad suffix, missing suffix, too short, multi-char
        // suffix, and zero-valued tokens. Each must fall through to the model
        // with no host-side cadence.
        for input in [
            "5x do x",                    // bad suffix
            "5 do x",                     // no suffix
            "m do x",                     // too short / no digits
            "55mm do x",                  // multi-char suffix
            "0m do x",                    // zero value (tool would reject)
            "0s do x",                    // zero value
            "abc do x",                   // alphabetic
            "99999999999999999999m do x", // overflows u64 -> parse Err branch
        ] {
            let (interval, prompt) = parse_loop_args(input);
            assert_eq!(interval, None, "input {input:?} must not yield a token");
            assert_eq!(prompt, input);
        }
    }

    #[test]
    fn natural_language_intervals_are_not_defaulted_host_side() {
        // The host no longer parses natural-language intervals or substitutes a
        // default — these all fall through to the model with no interval token.
        for input in [
            "every 30 minutes do x",
            "30 min check deploy",
            "1 hour run report",
            "run the report every 1h",
        ] {
            let (interval, prompt) = parse_loop_args(input);
            assert_eq!(interval, None, "input {input:?} must not yield a token");
            assert_eq!(prompt, input.trim());
        }
    }

    #[test]
    fn interval_to_human_formats() {
        assert_eq!(interval_to_human("5m"), "every 5 minutes");
        assert_eq!(interval_to_human("1m"), "every 1 minute");
        assert_eq!(interval_to_human("2h"), "every 2 hours");
        assert_eq!(interval_to_human("1h"), "every 1 hour");
        assert_eq!(interval_to_human("1d"), "every 1 day");
        assert_eq!(interval_to_human("7d"), "every 7 days");
        assert_eq!(interval_to_human("60s"), "every 60 seconds");
    }

    fn run_loop(args: &str) -> CommandResult {
        run_loop_with_background_loops(args, true)
    }

    fn run_loop_with_background_loops(args: &str, background_loops: bool) -> CommandResult {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot {
                scheduler_background_loops: background_loops,
                ..Default::default()
            },
        };
        LoopCommand.run(&mut ctx, args)
    }

    #[test]
    fn run_with_leading_token_shows_concrete_schedule() {
        match run_loop("30m check deploy status") {
            CommandResult::InjectSkill {
                scheduled_task_preview: Some(preview),
                ..
            } => {
                assert_eq!(preview.human_schedule, "every 30 minutes");
                assert_eq!(preview.prompt, "check deploy status");
            }
            other => panic!("expected InjectSkill with preview, got {other:?}"),
        }
    }

    #[test]
    fn run_without_leading_token_shows_placeholder_not_default() {
        match run_loop("check deploy status every 30 minutes") {
            CommandResult::InjectSkill {
                scheduled_task_preview: Some(preview),
                ..
            } => {
                // No fabricated cadence — the model fills in the real schedule.
                assert_eq!(preview.human_schedule, "scheduling…");
                assert_ne!(preview.human_schedule, "every 10 minutes");
                assert_eq!(preview.prompt, "check deploy status every 30 minutes");
            }
            other => panic!("expected InjectSkill with preview, got {other:?}"),
        }
    }

    #[test]
    fn run_bare_leading_token_shows_placeholder() {
        // "/loop 5m" with no prompt text: nothing to extract, so the preview
        // shows the placeholder and the whole input becomes the prompt.
        match run_loop("5m") {
            CommandResult::InjectSkill {
                scheduled_task_preview: Some(preview),
                ..
            } => {
                assert_eq!(preview.human_schedule, "scheduling…");
                assert_eq!(preview.prompt, "5m");
            }
            other => panic!("expected InjectSkill with preview, got {other:?}"),
        }
    }

    #[test]
    fn run_instruction_drops_host_default_and_explains_parsing() {
        match run_loop("every 30 minutes do x") {
            CommandResult::InjectSkill { prompt_blocks, .. } => {
                let acp::ContentBlock::Text(text) = &prompt_blocks[0] else {
                    panic!("expected a text prompt block");
                };
                let instruction = &text.text;
                assert!(
                    !instruction.contains("10m"),
                    "instruction must not advertise a 10m default: {instruction}"
                );
                // Stable, behaviour-bearing tokens, not incidental example text.
                assert!(instruction.contains("30 minutes"));
                assert!(instruction.contains("<number><unit>"));
                assert!(instruction.contains("ask the user how often"));
            }
            other => panic!("expected InjectSkill, got {other:?}"),
        }
    }

    #[test]
    fn run_empty_args_returns_usage_without_default_claim() {
        match run_loop("   ") {
            CommandResult::Message(msg) => {
                assert!(msg.contains("Usage: /loop"));
                assert!(
                    !msg.contains("10m"),
                    "usage must not claim a 10m default: {msg}"
                );
            }
            other => panic!("expected usage Message, got {other:?}"),
        }
    }

    // Drift guard (pager end): pager text == shared helper. With the shell's
    // `loop_prompt_matches_pager_wording`, this pins full shell↔pager parity.
    #[test]
    fn run_instruction_matches_shared_helper() {
        let args = "2h run tests";
        for (background_loops, mode) in [
            (true, LoopFireMode::Detached),
            (false, LoopFireMode::InSession),
        ] {
            match run_loop_with_background_loops(args, background_loops) {
                CommandResult::InjectSkill { prompt_blocks, .. } => {
                    let acp::ContentBlock::Text(text) = &prompt_blocks[0] else {
                        panic!("expected a text prompt block");
                    };
                    assert_eq!(text.text, loop_schedule_instruction(args, mode));
                }
                other => panic!("expected InjectSkill, got {other:?}"),
            }
        }
    }

    #[test]
    fn run_usage_matches_shared_helper() {
        match run_loop("   ") {
            CommandResult::Message(msg) => assert_eq!(msg, loop_usage_message()),
            other => panic!("expected usage Message, got {other:?}"),
        }
    }
}
