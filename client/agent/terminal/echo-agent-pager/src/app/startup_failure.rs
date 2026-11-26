//! Startup failures as data. [`StartupFailure::user_report`] is the only place
//! they become the text a reader sees.

mod render;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use echo_agent_telemetry::startup::{AgentKind, PhaseSnapshot, StartupOutcome, StartupPhase};

#[derive(Debug)]
pub struct StartupFailure {
    reason: Reason,
    context: Context,
}

#[derive(Debug)]
enum Reason {
    TimedOut {
        waited: Duration,
        timings: PhaseSnapshot,
    },
    Cancelled,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct EarlierAttempt {
    pub(crate) target: AgentKind,
    pub(crate) wait: Duration,
    pub(crate) outcome: StartupOutcome,
    pub(crate) longest_step: Option<StartupPhase>,
}

impl EarlierAttempt {
    fn shaped_the_wait(&self) -> bool {
        self.wait.as_secs() > 0
    }

    /// The leader is still running and still wedged, so clearing it is what
    /// stops the next start paying the same wait.
    fn wedged_leader(&self) -> bool {
        self.outcome == StartupOutcome::Timeout
            && self.longest_step == Some(StartupPhase::LeaderConnect)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ConnectAttempt {
    First,
    AfterFallback(EarlierAttempt),
}

impl ConnectAttempt {
    fn earlier(self) -> Option<EarlierAttempt> {
        match self {
            Self::First => None,
            Self::AfterFallback(earlier) => Some(earlier),
        }
    }

    fn earlier_wait(self) -> Duration {
        self.earlier()
            .map_or(Duration::ZERO, |earlier| earlier.wait)
    }
}

#[derive(Debug)]
pub(crate) struct Context {
    pub(crate) target: AgentKind,
    pub(crate) attempt: ConnectAttempt,
    pub(crate) version: String,
    pub(crate) log_path: PathBuf,
}

impl StartupFailure {
    pub(crate) fn timed_out(context: Context, waited: Duration, timings: PhaseSnapshot) -> Self {
        Self {
            reason: Reason::TimedOut {
                waited: waited + context.attempt.earlier_wait(),
                timings,
            },
            context,
        }
    }

    pub(crate) fn cancelled(context: Context) -> Self {
        Self {
            reason: Reason::Cancelled,
            context,
        }
    }

    pub fn user_report(&self) -> String {
        render::render(self)
    }
}

// One clause: this lands in log fields and `{e:#}` chains, where the full
// report does not belong.
impl fmt::Display for StartupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reason {
            Reason::TimedOut { waited, .. } => {
                write!(
                    f,
                    "startup timed out after {}",
                    render::whole_seconds(*waited)
                )
            }
            Reason::Cancelled => write!(f, "startup cancelled before an agent was ready"),
        }
    }
}

impl std::error::Error for StartupFailure {}
