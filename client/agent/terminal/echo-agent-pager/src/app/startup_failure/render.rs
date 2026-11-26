use std::fmt::Write as _;
use std::time::Duration;

use echo_agent_telemetry::startup::{AgentKind, PhaseSnapshot, StartupPhase, format_duration};

use crate::app::connect_timeout::CONNECT_UI_TIMEOUT_TRY_COMMAND;

use super::{ConnectAttempt, Context, EarlierAttempt, Reason, StartupFailure};

const WRAP_WIDTH: usize = 76;

pub(super) fn render(failure: &StartupFailure) -> String {
    let context = &failure.context;
    let mut rows = vec![
        ("Mode", attempted_agents(context)),
        ("Version", context.version.clone()),
    ];
    let mut report = match &failure.reason {
        Reason::TimedOut { waited, timings } => {
            let advice = advice_for(timings, context.attempt);
            rows.push(("Steps", format_steps(timings)));
            if let Some(command) = advice.next_step.command() {
                rows.push(("Try", command.to_owned()));
            }
            let explanation = fill_indented(&advice.explanation(), "  ", "  ");
            format!(
                "Couldn't start EchoAgent: startup timed out after {}.\n\n{explanation}",
                whole_seconds(*waited)
            )
        }
        Reason::Cancelled => format!(
            "Startup cancelled while connecting to the {}.",
            agent_name(context.target)
        ),
    };
    rows.push(("Log", context.log_path.display().to_string()));
    let _ = write!(report, "\n\n{}", label_rows(&rows));
    report
}

struct Advice {
    doing: Option<&'static str>,
    earlier: Option<EarlierAttempt>,
    next_step: NextStep,
}

/// A wedged leader is only ever the earlier attempt: the fallback that renders
/// this message never enters `LeaderConnect` itself.
fn advice_for(timings: &PhaseSnapshot, attempt: ConnectAttempt) -> Advice {
    let step = timings.longest_step().map(step_advice);
    let earlier = attempt.earlier();
    Advice {
        doing: step.map(|(doing, _)| doing),
        earlier: earlier.filter(|earlier| earlier.shaped_the_wait()),
        next_step: if earlier.is_some_and(|earlier| earlier.wedged_leader()) {
            NextStep::RestartSharedLeader
        } else {
            step.map_or(NextStep::Retry, |(_, next_step)| next_step)
        },
    }
}

impl Advice {
    fn explanation(&self) -> String {
        let mut explanation = match self.doing {
            Some(doing) => format!("The longest step was {doing}."),
            None => "No startup step had begun.".to_owned(),
        };
        if let Some(earlier) = self.earlier {
            let target = agent_name(earlier.target);
            let _ = write!(
                explanation,
                " EchoAgent spent the first {} on the {target}.",
                whole_seconds(earlier.wait)
            );
        }
        let _ = write!(explanation, " {}", self.next_step.text());
        // Only where waiting longer can help: a wedged leader never becomes
        // ready, so pairing this with "stop the leader" would contradict it.
        if matches!(
            self.next_step,
            NextStep::Retry | NextStep::CheckNetworkThenRetry
        ) {
            let _ = write!(
                explanation,
                " On a slow machine or network filesystem, a larger startup \
                 budget can help. Set it with the command below."
            );
        }
        explanation
    }
}

fn format_steps(timings: &PhaseSnapshot) -> String {
    let completed = timings
        .completed
        .iter()
        .map(|&(phase, elapsed)| (phase, elapsed, ""));
    let open = timings.open.map(|(phase, elapsed)| (phase, elapsed, "+"));
    let steps: Vec<String> = completed
        .chain(open)
        .map(|(phase, elapsed, still_running)| {
            format!(
                "{}={}{still_running}",
                phase.label(),
                format_duration(elapsed)
            )
        })
        .collect();
    if steps.is_empty() {
        return "none".to_owned();
    }
    steps.join(", ")
}

/// Values hang under their label, so a wrapped one never reads as a new field.
fn label_rows(rows: &[(&str, String)]) -> String {
    let column_width = rows
        .iter()
        .map(|(label, _)| label.len() + ":".len())
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|(label, value)| {
            let label = format!("  {:<column_width$} ", format!("{label}:"));
            let hanging = " ".repeat(label.len());
            fill_indented(value, &label, &hanging)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// A path or a command has to survive a paste, so words are never split.
fn fill_indented(text: &str, initial_indent: &str, subsequent_indent: &str) -> String {
    textwrap::fill(
        text,
        textwrap::Options::new(WRAP_WIDTH)
            .initial_indent(initial_indent)
            .subsequent_indent(subsequent_indent)
            .break_words(false)
            .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
    )
}

#[derive(Clone, Copy)]
enum NextStep {
    Retry,
    CheckNetworkThenRetry,
    RestartSharedLeader,
}

impl NextStep {
    fn text(self) -> &'static str {
        match self {
            Self::Retry => "Start EchoAgent again.",
            Self::CheckNetworkThenRetry => {
                "Check your network connection, then start EchoAgent again."
            }
            Self::RestartSharedLeader => {
                "Stop it with the command below, which also stops any other EchoAgent \
                 session using it, then start EchoAgent again."
            }
        }
    }

    /// Kept out of the prose so wrapping can never split it.
    fn command(self) -> Option<&'static str> {
        match self {
            Self::Retry | Self::CheckNetworkThenRetry => Some(CONNECT_UI_TIMEOUT_TRY_COMMAND),
            Self::RestartSharedLeader => Some("echoagent leader kill"),
        }
    }
}

/// Reads as the object of "The longest step was".
fn step_advice(phase: StartupPhase) -> (&'static str, NextStep) {
    use NextStep::{CheckNetworkThenRetry as Network, RestartSharedLeader, Retry};
    match phase {
        StartupPhase::ConfigLoad => ("reading your local configuration", Retry),
        StartupPhase::ManagedPolicy => ("checking your organization's managed policy", Network),
        StartupPhase::Bootstrap => ("loading your account settings", Network),
        // A disk cache read; the network fetch is the background refresh.
        StartupPhase::ModelCatalog => ("reading the list of available models", Retry),
        StartupPhase::WorkerSpawn => ("starting the local agent", Retry),
        // A Unix socket and a local spawn, never the network.
        StartupPhase::LeaderConnect => ("connecting to the shared leader", RestartSharedLeader),
        StartupPhase::AcpInitialize => ("waiting for the agent to respond", Retry),
        StartupPhase::EagerAuth => ("refreshing your sign-in", Network),
        StartupPhase::AppInit => ("preparing the interface", Retry),
        StartupPhase::SessionCreate => ("creating the session", Retry),
    }
}

fn attempted_agents(context: &Context) -> String {
    let target = agent_name(context.target);
    match context.attempt {
        ConnectAttempt::First => target.to_owned(),
        ConnectAttempt::AfterFallback(earlier) => {
            format!("{}, then {target}", agent_name(earlier.target))
        }
    }
}

fn agent_name(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Embedded => "local agent",
        AgentKind::Leader => "shared leader",
    }
}

/// Rounded: a truncated total can print smaller than the steps it sums.
pub(super) fn whole_seconds(wait: Duration) -> String {
    format!("{}s", (wait.as_millis() + 500) / 1000)
}
