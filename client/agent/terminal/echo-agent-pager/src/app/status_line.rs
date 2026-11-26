//! Status row state: whether this process draws a row at all, and for one that
//! does, its content, throttle, and which runs may still paint. What the row
//! should become is decided in `status_line_policy`, rendering is
//! `views::status_line`, and the counters are the `metrics` child.

use std::sync::Arc;
use std::time::{Duration, Instant};

use echo_agent_status_line::{StatusLineConfig, StatusLineContext, StatusLineTrigger};

use crate::app::actions::Effect;
use crate::app::agent::AgentId;
use crate::views::status_line::{RowSize, SanitizedText, StatusLineDisplay, StatusSegment};

mod command;
pub(crate) mod metrics;

/// Shortest gap between event-driven recomputes. A constant rather than a
/// config knob: the one cadence a user can set is the `refresh_interval`
/// timer, and this debounce only keeps a busy turn from re-running a script
/// `status_line_policy` call the throttle.
pub(crate) const EVENT_DEBOUNCE: Duration = Duration::from_millis(300);

pub(crate) const MIN_REFRESH_INTERVAL_MS: Duration = Duration::from_millis(100);

pub(crate) const ABANDON_AFTER: Duration = Duration::from_secs(30);

/// itself is broken and stops being papered over with stale data.
pub(crate) const REFRESH_FAILURES_TO_PAINT: u32 = 3;

const _: () = assert!(
    ABANDON_AFTER.as_secs() >= command::COMMAND_TIMEOUT.as_secs() * 2,
    "the watchdog must only fire for a task that never answers, never for a slow script"
);

/// Whether this process draws a row at all. Every gate that builds or streams
/// reads this, or one of them sends a payload nobody draws.
pub(crate) fn draws_a_row(config: &StatusLineConfig) -> bool {
    config.reserves_a_row()
}

/// `None` for empty output, which must not latch `had_content`.
fn display_for(text: &str) -> Option<StatusLineDisplay> {
    (!text.is_empty()).then(|| StatusLineDisplay::Text(SanitizedText::new(text)))
}

/// The parts of the payload the client fills in rather than the shell. The
/// overlay and the staleness check read this one value, so a field added here
/// is watched by the code that applies it. `trigger` is client-stamped too
/// but lives outside: it is a property of one run, not a staleness input the
/// row must rebuild over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ClientOwnedFields {
    pub(crate) session_name: Option<String>,
}

/// Identifies one run of the user's script, so a result that outlived the run
/// that asked for it can be told from the one the row is waiting on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    id: RunId,
    started: Instant,
    /// Rides the slot rather than the task result, so a result can never
    /// claim a trigger the run was not started with.
    trigger: StatusLineTrigger,
}

impl Run {
    fn past_deadline(self, now: Instant) -> bool {
        now.duration_since(self.started) >= ABANDON_AFTER
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RunState {
    #[default]
    Idle,
    Running(Run),
    Superseded(Run),
    /// A run past [`ABANDON_AFTER`]. The id is kept so a late result still
    /// paints, until the next run takes the slot.
    Abandoned(Run),
}

/// The state of the run slot. One value rather than a pair of predicates, so
/// no caller can read a run as both inside its deadline and past it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunSlot {
    Free,
    WithinDeadline,
    /// A run holds the slot past [`ABANDON_AFTER`]. Only the tick that runs
    /// [`StatusLineState::abandon_if_past_deadline`] hands the slot back.
    PastDeadline,
}

impl RunState {
    fn slot(self, now: Instant) -> RunSlot {
        match self {
            RunState::Running(run) | RunState::Superseded(run) if run.past_deadline(now) => {
                RunSlot::PastDeadline
            }
            RunState::Running(_) | RunState::Superseded(_) => RunSlot::WithinDeadline,
            // An abandoned run was counted and its slot handed back already.
            RunState::Idle | RunState::Abandoned(_) => RunSlot::Free,
        }
    }

    /// Abandon a run that has held the slot past [`ABANDON_AFTER`], counting
    /// it once. Returns the trigger of a run newly parked as `Abandoned`; a
    /// already re-raised when it was superseded.
    fn abandon_if_past_deadline(&mut self, now: Instant) -> Option<StatusLineTrigger> {
        let (next, run, abandoned_trigger) = match *self {
            // Dropped rather than re-armed: `Abandoned` keeps the id, which
            // would let a superseded run's output paint.
            RunState::Superseded(run) if run.past_deadline(now) => (RunState::Idle, run, None),
            RunState::Running(run) if run.past_deadline(now) => {
                (RunState::Abandoned(run), run, Some(run.trigger))
            }
            RunState::Idle
            | RunState::Running(_)
            | RunState::Superseded(_)
            | RunState::Abandoned(_) => return None,
        };
        tracing::warn!(run_id = run.id.0, "status_line: run abandoned, no result");
        metrics::global().record_abandoned();
        *self = next;
        abandoned_trigger
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForcePolicy {
    Clear,
    /// The force was raised while this update's work was already running, so
    /// this update does not satisfy it.
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterSupersede {
    Rerun,
    /// Nothing should run, so no force is left standing to demand ticks.
    NoRun,
}

#[derive(Debug)]
pub struct StatusLineRun {
    id: RunId,
    command: String,
    ctx: Box<StatusLineContext>,
    term_size: RowSize,
}

/// What one run produced. A failure carries both the text it would paint and
/// the raw error for the log, so the state can decide which run gets which
/// without re-deriving either.
#[derive(Debug)]
pub enum RunOutcome {
    Output(String),
    Failed {
        /// The `[status line: …]` text a state-triggered run paints.
        text: String,
        /// The bare error, for the unified log.
        error: String,
    },
}

/// What [`StatusLineState::finish_command_run`] did with a result, so the
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FinishDisposition {
    Applied,
    RefreshFailureKept { error: String, failures: u32 },
    RefreshFailurePainted { error: String, failures: u32 },
}

#[derive(Default)]
pub(crate) struct StatusLineState {
    content: Option<Arc<StatusLineDisplay>>,
    settled: bool,
    answered: bool,
    last_update: Option<Instant>,
    forced: bool,
    refresh_due: bool,
    /// that succeeds. The configured command is fixed for the life of the
    /// process, so the count deliberately survives [`Self::invalidate`]: an
    /// agent switch does not absolve a broken script. A future config reload
    /// must reset it when the command changes.
    refresh_failures: u32,
    run: RunState,
    next_run_id: RunId,
    changed: bool,
    source: Option<AgentId>,
    built_from: ClientOwnedFields,
}

impl StatusLineState {
    pub(crate) fn source(&self) -> Option<AgentId> {
        self.source
    }

    pub(crate) fn client_fields(&self) -> &ClientOwnedFields {
        &self.built_from
    }

    /// Returns whether they changed. Unlike [`Self::set_source`] the content
    /// stands: a renamed session is still this session, so the row is stale.
    #[must_use = "a change needs the rebuild the caller was going to force"]
    pub(crate) fn set_client_fields(&mut self, current: ClientOwnedFields) -> bool {
        if self.built_from == current {
            return false;
        }
        self.built_from = current;
        true
    }

    /// Returns whether the row changed agents.
    pub(crate) fn set_source(&mut self, source: Option<AgentId>) -> bool {
        if self.source == source {
            return false;
        }
        self.source = source;
        self.invalidate();
        true
    }

    pub(crate) fn display(&self) -> Option<Arc<StatusLineDisplay>> {
        self.content.clone()
    }

    pub(crate) fn is_settled(&self) -> bool {
        self.settled
    }

    /// Settle with no content, leaving `last_update` alone so a later snapshot
    /// paints at once. A force left standing would demand ticks forever, and
    pub(crate) fn settle_empty(&mut self) {
        self.settled = true;
        self.clear_force();
        self.refresh_due = false;
    }

    #[must_use = "dropping the flag loses the redraw it was asking for"]
    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub(crate) fn is_due(&self, now: Instant) -> bool {
        // already served by the timer that raised it.
        let interval = if self.forced || self.refresh_due {
            MIN_REFRESH_INTERVAL_MS
        } else {
            EVENT_DEBOUNCE
        };
        self.last_update
            .is_none_or(|at| now.duration_since(at) >= interval)
    }

    pub(crate) fn stamp(&mut self, now: Instant, force: ForcePolicy) {
        self.last_update = Some(now);
        match force {
            ForcePolicy::Clear => self.clear_force(),
            ForcePolicy::Keep => {}
        }
    }

    fn clear_force(&mut self) {
        self.forced = false;
    }

    /// Drops the next run to the floor rather than clearing the throttle: a
    /// window drag would otherwise run a script per frame.
    pub(crate) fn force_next_run(&mut self) {
        self.forced = true;
    }

    pub(crate) fn force_pending(&self) -> bool {
        self.forced
    }

    pub(crate) fn request_refresh(&mut self) {
        self.refresh_due = true;
    }

    pub(crate) fn cancel_refresh_request(&mut self) {
        self.refresh_due = false;
    }

    pub(crate) fn refresh_due(&self) -> bool {
        self.refresh_due
    }

    pub(crate) fn abandon_if_past_deadline(&mut self, now: Instant) {
        // must not swallow the cycle the timer scheduled.
        if self.run.abandon_if_past_deadline(now) == Some(StatusLineTrigger::RefreshInterval) {
            self.refresh_due = true;
        }
    }

    pub(crate) fn run_slot(&self, now: Instant) -> RunSlot {
        self.run.slot(now)
    }

    /// A run still holds the slot, inside its deadline or past it. `Abandoned`
    /// does not.
    pub(crate) fn command_in_flight(&self, now: Instant) -> bool {
        self.run_slot(now) != RunSlot::Free
    }

    #[must_use = "the effect must be dispatched, or the row waits forever on a run that never started"]
    pub(crate) fn begin_command_run(
        &mut self,
        now: Instant,
        command: String,
        mut ctx: Box<StatusLineContext>,
        term_size: RowSize,
    ) -> Option<Effect> {
        if self.command_in_flight(now) {
            return None;
        }
        let trigger = if std::mem::take(&mut self.refresh_due) {
            StatusLineTrigger::RefreshInterval
        } else {
            StatusLineTrigger::State
        };
        ctx.trigger = Some(trigger);
        let id = self.next_run_id;
        self.next_run_id.0 += 1;
        self.run = RunState::Running(Run {
            id,
            started: now,
            trigger,
        });
        self.stamp(now, ForcePolicy::Clear);
        Some(Effect::RunStatusLineCommand(StatusLineRun {
            id,
            command,
            ctx,
            term_size,
        }))
    }

    /// A superseded run's output is dropped rather than painted.
    /// [`AfterSupersede`] already settled what replaces it.
    #[must_use = "the disposition carries the refresh failure the caller must log"]
    pub(crate) fn finish_command_run(
        &mut self,
        now: Instant,
        id: RunId,
        outcome: RunOutcome,
    ) -> FinishDisposition {
        // No watchdog pass first: the id is better evidence than the elapsed time.
        let disposition = match self.run {
            RunState::Running(run) if run.id == id => {
                self.run = RunState::Idle;
                self.apply_run_outcome(run.trigger, outcome)
            }
            RunState::Abandoned(run) if run.id == id => {
                self.run = RunState::Idle;
                if run.trigger == StatusLineTrigger::RefreshInterval {
                    self.refresh_due = false;
                }
                self.apply_run_outcome(run.trigger, outcome)
            }
            RunState::Superseded(run) if run.id == id => {
                self.run = RunState::Idle;
                FinishDisposition::Applied
            }
            // A result no run is owed moves nothing, `last_update` included.
            RunState::Idle
            | RunState::Abandoned(_)
            | RunState::Running(_)
            | RunState::Superseded(_) => return FinishDisposition::Applied,
        };
        // The interval runs from the end of this run, not from its start, or a
        // script slower than the interval re-runs with no gap.
        self.stamp(now, ForcePolicy::Keep);
        disposition
    }

    fn apply_run_outcome(
        &mut self,
        trigger: StatusLineTrigger,
        outcome: RunOutcome,
    ) -> FinishDisposition {
        match outcome {
            RunOutcome::Output(line) => {
                self.refresh_failures = 0;
                self.answered = true;
                self.settle_with_session_content(display_for(&line));
                FinishDisposition::Applied
            }
            RunOutcome::Failed { text, error } => match trigger {
                StatusLineTrigger::State => {
                    self.settle_with_session_content(display_for(&text));
                    FinishDisposition::Applied
                }
                StatusLineTrigger::RefreshInterval => {
                    self.refresh_failures = self.refresh_failures.saturating_add(1);
                    let failures = self.refresh_failures;
                    if failures >= REFRESH_FAILURES_TO_PAINT || !self.answered {
                        self.settle_with_session_content(display_for(&text));
                        FinishDisposition::RefreshFailurePainted { error, failures }
                    } else {
                        // Settled without touching the content: the row keeps
                        // its last answer rather than waiting on this one.
                        self.settled = true;
                        FinishDisposition::RefreshFailureKept { error, failures }
                    }
                }
            },
        }
    }

    /// Supersede the outstanding run so its output can no longer paint. The
    /// caller says what replaces it, since this destroys the only thing that
    /// would have refreshed the row.
    pub(crate) fn supersede_command_run(&mut self, after: AfterSupersede) {
        self.run = match self.run {
            RunState::Running(run) => {
                // scheduled until the next fire.
                if run.trigger == StatusLineTrigger::RefreshInterval {
                    self.refresh_due = true;
                }
                RunState::Superseded(run)
            }
            RunState::Abandoned(_) => RunState::Idle,
            state @ (RunState::Superseded(_) | RunState::Idle) => state,
        };
        // Outside the match: a caller that wants no run is obeyed even when there
        // was none, or a stale force demands ticks forever.
        match after {
            AfterSupersede::Rerun => self.force_next_run(),
            AfterSupersede::NoRun => self.clear_force(),
        }
    }

    pub(crate) fn set_segments(&mut self, segments: Vec<StatusSegment>) {
        self.settle_with_session_content(
            (!segments.is_empty()).then_some(StatusLineDisplay::Segments(segments)),
        );
    }

    /// About the config rather than the session, so it does not count as content.
    pub(crate) fn set_problem(&mut self, text: &str) {
        self.settled = true;
        // A segment for the warning tone: the rest of the row is chrome, and a row
        // that cannot read its own config is the one thing the user must notice.
        self.write_content(Some(StatusLineDisplay::Segments(vec![
            StatusSegment::warn(text),
        ])));
    }

    /// An answer about the session, which is what `had_content` reports. Counted
    /// here rather than where a script prints: a superseded run never lands.
    fn settle_with_session_content(&mut self, next: Option<StatusLineDisplay>) {
        if next.is_some() {
            metrics::global().note_content();
        }
        self.settled = true;
        self.write_content(next);
    }

    /// The only writer of `content`, so no setter can miss the redraw flag.
    fn write_content(&mut self, next: Option<StatusLineDisplay>) {
        if self.content.as_deref() != next.as_ref() {
            self.content = next.map(Arc::new);
            self.changed = true;
        }
    }

    /// Clear the content and leave the row unsettled, so ticks continue until an
    /// answer arrives. Contrast [`Self::settle_empty`]; the force goes with it,
    /// screen had no agent is kept for the first run after one appears.
    pub(crate) fn invalidate(&mut self) {
        self.supersede_command_run(AfterSupersede::NoRun);
        self.write_content(None);
        self.settled = false;
        self.answered = false;
    }
}

#[cfg(test)]
pub(crate) fn test_context(cwd: &str) -> StatusLineContext {
    use echo_agent_status_line::StatusLineWorkspace;

    StatusLineContext {
        cwd: cwd.to_string(),
        workspace: StatusLineWorkspace {
            current_dir: cwd.to_string(),
            repo_root: Some(cwd.to_string()),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod tests;
