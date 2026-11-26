//! `/workflow` -- pager-side wrapper over the shell's workflow command.
//!
//! Registered as a builtin so it shadows the ACP-advertised `/workflow`
//! (`apply_acp_commands` drops colliding names): the exact `runs` form can
//! then open the TUI run dashboard, while every other form passes through
//! to the shell unchanged (launch, manage ops, bare-call text overview).
//!
//! Argument suggestions list advertised workflow names (from the ACP
//! catalog) then the manage ops. Selecting a launch name fills
//! `/workflow <name> ` without launching. Selecting pause/resume/stop/save
//! lists this session's run handles so a bare verb cannot pick a run.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

fn first_phase_items(ctx: &AppCtx) -> Vec<ArgItem> {
    let mut items: Vec<ArgItem> = ctx
        .saved_workflows
        .iter()
        .map(|workflow| ArgItem {
            display: workflow.name.clone(),
            match_text: workflow.name.clone(),
            insert_text: format!("{} ", workflow.name),
            description: workflow.description.clone(),
        })
        .collect();
    items.extend(WORKFLOW_OPS.iter().map(|&(op, description)| {
        let insert_text = if op == "runs" {
            op.to_string()
        } else {
            format!("{op} ")
        };
        ArgItem {
            display: op.to_string(),
            match_text: op.to_string(),
            insert_text,
            description: description.to_string(),
        }
    }));
    items
}

fn is_manage_op(op: &str) -> bool {
    matches!(
        op.to_ascii_lowercase().as_str(),
        "pause" | "resume" | "stop" | "save"
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LaunchFlag {
    AgentBudget,
    Effort,
}

#[derive(Clone, Copy)]
enum LaunchValueProvider {
    Opaque,
    ReasoningEffort,
}

#[derive(Clone, Copy)]
struct LaunchFlagSpec {
    flag: LaunchFlag,
    name: &'static str,
    description: &'static str,
    value_provider: LaunchValueProvider,
}

const LAUNCH_FLAG_SPECS: [LaunchFlagSpec; 2] = [
    LaunchFlagSpec {
        flag: LaunchFlag::AgentBudget,
        name: "--agent-budget",
        description: "Set the cumulative child-agent cap (1–1,024)",
        value_provider: LaunchValueProvider::Opaque,
    },
    LaunchFlagSpec {
        flag: LaunchFlag::Effort,
        name: "--effort",
        description: "",
        value_provider: LaunchValueProvider::ReasoningEffort,
    },
];

#[derive(Clone, Copy)]
enum LaunchValueSyntax {
    Separate,
    Equals,
}

enum LaunchFlagState {
    Flags {
        used: Vec<LaunchFlag>,
        completed_args: String,
        prefix: String,
    },
    Value {
        used: Vec<LaunchFlag>,
        completed_args: String,
        spec: &'static LaunchFlagSpec,
        syntax: LaunchValueSyntax,
    },
    Closed,
}

#[derive(Clone, Copy)]
enum LaunchValueCompletion {
    Prefix,
    Complete,
}

impl LaunchFlagSpec {
    fn matches_value(self, ctx: &AppCtx, value: &str, completion: LaunchValueCompletion) -> bool {
        match self.value_provider {
            LaunchValueProvider::Opaque => matches!(completion, LaunchValueCompletion::Complete),
            LaunchValueProvider::ReasoningEffort => ctx
                .models
                .reasoning_effort_options()
                .into_iter()
                .any(|option| {
                    [option.value.to_string(), option.id, option.label]
                        .into_iter()
                        .any(|candidate| match completion {
                            LaunchValueCompletion::Prefix => candidate
                                .to_ascii_lowercase()
                                .starts_with(&value.to_ascii_lowercase()),
                            LaunchValueCompletion::Complete => {
                                candidate.eq_ignore_ascii_case(value)
                            }
                        })
                }),
        }
    }

    fn value_items(self, ctx: &AppCtx, base: &str, syntax: LaunchValueSyntax) -> Vec<ArgItem> {
        match self.value_provider {
            LaunchValueProvider::Opaque => vec![ArgItem {
                display: self.name.to_string(),
                match_text: format!("{base} {}", self.name),
                insert_text: format!("{base} {} ", self.name),
                description: self.description.to_string(),
            }],
            LaunchValueProvider::ReasoningEffort => ctx
                .models
                .reasoning_effort_options()
                .into_iter()
                .map(|option| {
                    let canonical = option.value.to_string();
                    let option_description = option.description.unwrap_or_default();
                    let description = if option.label.eq_ignore_ascii_case(&canonical) {
                        option_description
                    } else if option_description.is_empty() {
                        option.label.clone()
                    } else {
                        format!("{} — {option_description}", option.label)
                    };
                    let argument = match syntax {
                        LaunchValueSyntax::Separate => format!("{} {canonical}", self.name),
                        LaunchValueSyntax::Equals => format!("{}={canonical}", self.name),
                    };
                    ArgItem {
                        display: format!("{} {canonical}", self.name),
                        match_text: format!("{base} {argument} {} {}", option.id, option.label),
                        insert_text: format!("{base} {argument} "),
                        description,
                    }
                })
                .collect(),
        }
    }
}

fn parse_launch_flags(ctx: &AppCtx, rest: &str) -> LaunchFlagState {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let has_trailing_whitespace = rest.chars().next_back().is_some_and(char::is_whitespace);
    let mut used = Vec::new();
    let mut index = 0;

    loop {
        if index == tokens.len() {
            return if LAUNCH_FLAG_SPECS
                .iter()
                .all(|spec| used.contains(&spec.flag))
            {
                LaunchFlagState::Closed
            } else {
                LaunchFlagState::Flags {
                    used,
                    completed_args: tokens.join(" "),
                    prefix: String::new(),
                }
            };
        }

        let token = tokens[index];
        let (flag_name, equals_value) = token
            .split_once('=')
            .map_or((token, None), |(name, value)| (name, Some(value)));
        let Some(spec) = LAUNCH_FLAG_SPECS.iter().find(|spec| spec.name == flag_name) else {
            if index + 1 != tokens.len()
                || has_trailing_whitespace
                || !LAUNCH_FLAG_SPECS
                    .iter()
                    .filter(|spec| !used.contains(&spec.flag))
                    .any(|spec| spec.name.starts_with(token))
            {
                return LaunchFlagState::Closed;
            }
            return LaunchFlagState::Flags {
                used,
                completed_args: tokens[..index].join(" "),
                prefix: token.to_string(),
            };
        };
        if used.contains(&spec.flag) {
            return LaunchFlagState::Closed;
        }

        if let Some(value) = equals_value {
            if value.is_empty() {
                return LaunchFlagState::Closed;
            }
            if index + 1 == tokens.len() && !has_trailing_whitespace {
                return if spec.matches_value(ctx, value, LaunchValueCompletion::Prefix) {
                    LaunchFlagState::Value {
                        used,
                        completed_args: tokens[..index].join(" "),
                        spec,
                        syntax: LaunchValueSyntax::Equals,
                    }
                } else {
                    LaunchFlagState::Closed
                };
            }
            if !spec.matches_value(ctx, value, LaunchValueCompletion::Complete) {
                return LaunchFlagState::Closed;
            }
            used.push(spec.flag);
            index += 1;
            continue;
        }

        if index + 1 == tokens.len() {
            return match spec.value_provider {
                LaunchValueProvider::ReasoningEffort => LaunchFlagState::Value {
                    used,
                    completed_args: tokens[..index].join(" "),
                    spec,
                    syntax: LaunchValueSyntax::Separate,
                },
                LaunchValueProvider::Opaque if !has_trailing_whitespace => LaunchFlagState::Flags {
                    used,
                    completed_args: tokens[..index].join(" "),
                    prefix: token.to_string(),
                },
                LaunchValueProvider::Opaque => LaunchFlagState::Closed,
            };
        }

        let value = tokens[index + 1];
        if index + 2 == tokens.len() && !has_trailing_whitespace {
            return if spec.matches_value(ctx, value, LaunchValueCompletion::Prefix) {
                LaunchFlagState::Value {
                    used,
                    completed_args: tokens[..index].join(" "),
                    spec,
                    syntax: LaunchValueSyntax::Separate,
                }
            } else {
                LaunchFlagState::Closed
            };
        }
        if !spec.matches_value(ctx, value, LaunchValueCompletion::Complete) {
            return LaunchFlagState::Closed;
        }
        used.push(spec.flag);
        index += 2;
    }
}

fn launch_flag_items(
    ctx: &AppCtx,
    name: &str,
    completed_args: &str,
    flag_prefix: &str,
    used: &[LaunchFlag],
    value_spec: Option<(&LaunchFlagSpec, LaunchValueSyntax)>,
) -> Vec<ArgItem> {
    let base = if completed_args.is_empty() {
        name.to_string()
    } else {
        format!("{name} {completed_args}")
    };
    LAUNCH_FLAG_SPECS
        .iter()
        .filter(|spec| !used.contains(&spec.flag))
        .filter(|spec| {
            value_spec.is_some_and(|(value_spec, _)| value_spec.name == spec.name)
                || spec.name.starts_with(flag_prefix)
        })
        .flat_map(|spec| {
            let syntax = value_spec
                .filter(|(value_spec, _)| value_spec.name == spec.name)
                .map_or(LaunchValueSyntax::Separate, |(_, syntax)| syntax);
            spec.value_items(ctx, &base, syntax)
        })
        .collect()
}

fn manage_run_items(ctx: &AppCtx, op: &str) -> Vec<ArgItem> {
    let op = op.to_ascii_lowercase();
    ctx.workflow_runs
        .iter()
        .filter(|run| match op.as_str() {
            "pause" => run.can_pause(),
            "resume" => run.can_resume(),
            "stop" => run.can_stop(),
            "save" => run.can_save(ctx.saved_workflows),
            _ => true,
        })
        .map(|run| {
            // match_text must include the verb: the controller ranks the
            // full args query (`resume rev`) against this string.
            let insert_text = format!("{op} {}", run.name);
            ArgItem {
                display: run.name.clone(),
                match_text: insert_text.clone(),
                insert_text,
                description: run.status.replace('_', " "),
            }
        })
        .collect()
}

/// Ops the shell accepts, offered after advertised workflow names.
const WORKFLOW_OPS: [(&str, &str); 5] = [
    (
        "runs",
        "Show workflow runs (dashboard; text overview in minimal)",
    ),
    ("pause", "Pause a running workflow"),
    ("resume", "Resume a paused workflow"),
    ("stop", "Stop a workflow run"),
    ("save", "Save a run's script as a named workflow"),
];

/// `/workflow runs` toggles the run dashboard outside minimal mode; all
/// other forms forward to the shell.
pub struct WorkflowCommand;

impl SlashCommand for WorkflowCommand {
    fn name(&self) -> &str {
        "workflow"
    }

    fn description(&self) -> &str {
        // Mirrors the shell builtin this command shadows.
        "Launch a saved workflow, list runs, or manage a run (pause, resume, stop, save)"
    }

    fn usage(&self) -> &str {
        "/workflow"
    }

    /// Offered only while the shell advertises workflow support, mirroring
    /// the ACP entry this builtin shadows.
    fn visible(&self, ctx: &AppCtx) -> bool {
        ctx.workflows_available
    }

    fn session_scoped(&self) -> bool {
        true
    }

    // Shadows ACP `/workflow` (`has_args: true`); false drops the placeholder and highlighted op rows.
    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some(
            "<name> [--agent-budget N] [--effort LEVEL] [args] | runs | pause|resume|stop|save [name]",
        )
    }

    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let trimmed = args_query.trim_start();
        // First token is still being typed (`/workflow de`). Leave the
        // name+op list up so the matcher can rank saved names.
        let Some((first, rest)) = trimmed.split_once(char::is_whitespace) else {
            // Exact verb (`/workflow resume`) lists this session's runs —
            // not saved launch names. A prefix stays first-phase.
            if is_manage_op(trimmed) {
                return Some(manage_run_items(ctx, trimmed));
            }
            return Some(first_phase_items(ctx));
        };
        let rest = rest.trim_start();
        if is_manage_op(first) {
            if rest.contains(char::is_whitespace) {
                return None;
            }
            return Some(manage_run_items(ctx, first));
        }
        if ctx
            .saved_workflows
            .iter()
            .any(|workflow| workflow.name.eq_ignore_ascii_case(first))
        {
            let (completed_args, flag_prefix, used, value_spec) =
                match parse_launch_flags(ctx, rest) {
                    LaunchFlagState::Flags {
                        used,
                        completed_args,
                        prefix,
                    } => (completed_args, prefix, used, None),
                    LaunchFlagState::Value {
                        used,
                        completed_args,
                        spec,
                        syntax,
                    } => (
                        completed_args,
                        spec.name.to_string(),
                        used,
                        Some((spec, syntax)),
                    ),
                    LaunchFlagState::Closed => return None,
                };
            let items =
                launch_flag_items(ctx, first, &completed_args, &flag_prefix, &used, value_spec);
            return (!items.is_empty()).then_some(items);
        }
        // Unknown launch name or `runs` already chosen.
        None
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        // Case-insensitive like the shell's `runs` op. Fullscreen and inline
        // both render the pane (same non-minimal set FullscreenOnly gates
        // on); minimal falls through to the shell's text overview.
        if trimmed.eq_ignore_ascii_case("runs") && !ctx.screen_mode.is_minimal() {
            return CommandResult::Action(Action::ToggleWorkflows);
        }
        if trimmed.is_empty() {
            return CommandResult::PassThrough("/workflow".to_string());
        }
        CommandResult::PassThrough(format!("/workflow {args}"))
    }
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
