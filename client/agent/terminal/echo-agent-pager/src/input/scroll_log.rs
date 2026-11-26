//! Scroll flight recorder — `ECHO_AGENT_SCROLL_LOG` JSONL log of scroll-stream
//! transitions, for offline analysis of real gestures.
//!
//! The scroll-debug HUD ([`crate::views::scroll_debug_hud`]) samples state
//! per frame; this recorder captures every state-machine transition
//! event-exactly: one line per stream start, per line-delivering flush
//! (zero-delta flush attempts are not logged — their spacing shows up in
//! `ms_since_prev_flush`), and per finalize. Records are flat JSON objects,
//! one per line, and the writer flushes on finalize records so `tail -f` +
//! `jq` work mid-session. In captures from before the finalize-decel fix, a
//! tick-path finalize with a large `flushed` and nonzero `dropped` long
//! after the last `evt="flush"` line is the rear-end burst signature; fixed
//! producers drain tapered `trigger="tick"` flushes instead and finalize
//! with `flushed: 0`, where `dropped` counts only coast-budget write-offs
//! (see [`super::mouse`]).
//!
//! Enablement: `ECHO_AGENT_SCROLL_LOG=1` (or set-but-empty) logs to
//! `~/.echo-agent/logs/scroll-log-<timestamp>.jsonl`; any other non-`0` value is
//! used as the target path. Unset (or `0`, matching `ECHO_AGENT_SCROLL_DEBUG`)
//! disables: [`super::mouse::MouseScrollState`] then holds `None` and every
//! emission point costs one branch.
//!
//! Invariant (same contract as the HUD): pure observation — the recorder is
//! write-only for the state machine and never feeds back into scroll
//! behavior. IO failures drop the record and disable the recorder with a
//! single `tracing::warn!` (never stderr — that is the TUI's terminal), and
//! never panic.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

/// Record type: which state-machine transition produced the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScrollLogEvt {
    /// A new stream was created; its `carry` field shows the sub-line
    /// remainder that rode in from the previous same-direction stream.
    StreamStart,
    /// A flush delivered lines mid-stream.
    Flush,
    /// The stream ended (80ms gap or direction flip): `flushed` is the
    /// tapered catch-up flush (0 once the post-gap drain ran dry), `dropped`
    /// the whole-line backlog discarded with the stream (flip cancellations
    /// and coast-budget write-offs).
    Finalize,
}

/// Code path that emitted the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScrollLogTrigger {
    /// `on_scroll_event`: stream start, or its 16ms-cadence flush.
    Event,
    /// `on_tick` cadence flush (scroll-clock wakeup between events).
    Tick,
    /// Immediate flush when Auto-mode wheel promotion fired.
    Promotion,
    /// The capped flush inside stream finalize. Detection path is
    /// recoverable: a finalize followed at the same `ts_ms` by a
    /// `stream_start` came from the event path (flip/regrasp); one with no
    /// successor came from the tick path (fingers stopped).
    Finalize,
}

/// Config echo carried by `stream_start` records only: attributes the
/// gesture to a playground variant offline (speed/lines are otherwise
/// confounded inside `desired`/`accel`). Per-flush records skip it.
/// `ept`/`lpt` abbreviate events/lines per tick; `mode` is the
/// [`super::mouse::ScrollInputMode`] label in effect.
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct ScrollLogConfigEcho {
    pub mode: &'static str,
    pub ept: u16,
    pub wheel_lpt: u16,
    pub trackpad_lpt: u16,
    pub invert: bool,
    pub speed: f32,
    pub viewport_height: u16,
}

/// Per-record facts supplied by the state machine ([`super::mouse`]); the
/// recorder adds the bookkeeping fields (`ts_ms`, `events_since_flush`,
/// `ms_since_prev_flush`) when building the [`ScrollLogRecord`].
pub(crate) struct ScrollLogEvent {
    pub evt: ScrollLogEvt,
    pub trigger: ScrollLogTrigger,
    /// Raw stream classification (`unknown` until promotion/finalize).
    pub kind: &'static str,
    /// Events accumulated in the stream so far.
    pub events_total: usize,
    /// Rolling average inter-event interval (ms); `None` until two
    /// accel-countable events arrived.
    pub avg_interval_ms: Option<f32>,
    /// Acceleration multiplier in effect.
    pub accel: f32,
    /// Fractional target lines (post accel/speed multipliers, carry included).
    pub desired: f32,
    /// Whole lines delivered for this stream so far (post-flush).
    pub applied_total: i32,
    /// Lines this record's flush delivered (0 on stream_start).
    pub flushed: i32,
    /// Whole-line backlog remaining after this record's flush.
    pub backlog_after: i32,
    /// Sub-line remainder included in `desired`.
    pub carry: f32,
    /// Per-flush delta cap in effect.
    pub cap: i32,
    /// Finalize only: whole lines discarded with the stream (equals
    /// `backlog_after` there by construction).
    pub dropped: Option<i32>,
    /// Stream-start only: the config captured on the stream.
    pub config: Option<ScrollLogConfigEcho>,
}

/// One serialized JSONL line: [`ScrollLogEvent`] plus recorder-computed
/// `ts_ms` (monotonic ms since recorder start), `events_since_flush`
/// (arrivals since the last logged flush/finalize of this stream), and
/// `ms_since_prev_flush` (spacing from the previous flush-bearing record;
/// absent before the first).
#[derive(Serialize)]
struct ScrollLogRecord {
    ts_ms: f64,
    evt: ScrollLogEvt,
    trigger: ScrollLogTrigger,
    kind: &'static str,
    events_total: usize,
    events_since_flush: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    avg_interval_ms: Option<f32>,
    accel: f32,
    desired: f32,
    applied_total: i32,
    flushed: i32,
    backlog_after: i32,
    carry: f32,
    cap: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    ms_since_prev_flush: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dropped: Option<i32>,
    // Flattened so the config echo stays flat JSON; None emits nothing.
    #[serde(flatten)]
    config: Option<ScrollLogConfigEcho>,
}

/// Lazily-opened JSONL sink; `Disabled` after the first IO failure.
#[derive(Debug)]
enum Sink {
    Pending(PathBuf),
    Open(BufWriter<File>),
    Disabled,
}

/// Appends [`ScrollLogRecord`]s to the `ECHO_AGENT_SCROLL_LOG` file. Owned as
/// `Option<Self>` by [`super::mouse::MouseScrollState`]; construction reads
/// the env once, the file opens on the first record so an enabled-but-idle
/// session creates nothing.
#[derive(Debug)]
pub(crate) struct ScrollLogRecorder {
    /// Time origin for `ts_ms`; the state machine's construction instant
    /// (tests: the synthetic timeline), or the toggle instant for a
    /// `/debug log` runtime-enabled recorder — self-consistent either way.
    base: Instant,
    sink: Sink,
    /// Emission time of the previous flush-bearing record.
    last_flush_at: Option<Instant>,
    /// `events_total` at the last logged flush/finalize (reset per stream).
    events_at_last_flush: usize,
}

impl ScrollLogRecorder {
    /// Build from `ECHO_AGENT_SCROLL_LOG` (see module docs for value semantics);
    /// `None` when unset or `0`.
    pub(crate) fn from_env_at(base: Instant) -> Option<Self> {
        let raw = std::env::var("ECHO_AGENT_SCROLL_LOG").ok()?;
        let value = raw.trim();
        if value == "0" {
            return None;
        }
        let path = if value.is_empty() || value == "1" {
            default_log_path()
        } else {
            PathBuf::from(value)
        };
        Some(Self::new(path, base))
    }

    /// Recorder targeting an explicit path (tests inject a tempfile here).
    pub(crate) fn new(path: PathBuf, base: Instant) -> Self {
        Self {
            base,
            sink: Sink::Pending(path),
            last_flush_at: None,
            events_at_last_flush: 0,
        }
    }

    /// Append one record. `now` must be the same instant the state machine
    /// used for the transition, so the log is exactly its timeline.
    pub(crate) fn record(&mut self, now: Instant, event: ScrollLogEvent) {
        if matches!(self.sink, Sink::Disabled) {
            return;
        }
        let events_since_flush = if event.evt == ScrollLogEvt::StreamStart {
            self.events_at_last_flush = 0;
            0
        } else {
            event.events_total.saturating_sub(self.events_at_last_flush)
        };
        let record = ScrollLogRecord {
            ts_ms: now.saturating_duration_since(self.base).as_secs_f64() * 1000.0,
            evt: event.evt,
            trigger: event.trigger,
            kind: event.kind,
            events_total: event.events_total,
            events_since_flush,
            avg_interval_ms: event.avg_interval_ms,
            accel: event.accel,
            desired: event.desired,
            applied_total: event.applied_total,
            flushed: event.flushed,
            backlog_after: event.backlog_after,
            carry: event.carry,
            cap: event.cap,
            ms_since_prev_flush: self
                .last_flush_at
                .map(|at| now.saturating_duration_since(at).as_secs_f64() * 1000.0),
            dropped: event.dropped,
            config: event.config,
        };
        if event.evt != ScrollLogEvt::StreamStart {
            self.last_flush_at = Some(now);
            self.events_at_last_flush = event.events_total;
        }
        let Ok(line) = serde_json::to_string(&record) else {
            return;
        };
        self.write_line(&line, event.evt == ScrollLogEvt::Finalize);
    }

    fn write_line(&mut self, line: &str, flush_now: bool) {
        if let Sink::Pending(path) = &self.sink {
            match open_writer(path) {
                Ok(writer) => self.sink = Sink::Open(writer),
                Err(err) => {
                    tracing::warn!(error = %err, "scroll log disabled: open failed");
                    self.sink = Sink::Disabled;
                    return;
                }
            }
        }
        let Sink::Open(writer) = &mut self.sink else {
            return;
        };
        let result = writeln!(writer, "{line}").and_then(|()| {
            // Finalize marks a gesture boundary: surface it to tail -f.
            if flush_now { writer.flush() } else { Ok(()) }
        });
        if let Err(err) = result {
            tracing::warn!(error = %err, "scroll log disabled: write failed");
            self.sink = Sink::Disabled;
        }
    }
}

fn open_writer(path: &Path) -> std::io::Result<BufWriter<File>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(BufWriter::new(File::create(path)?))
}

/// `~/.echo-agent/logs/scroll-log-<utc-ts>.jsonl` — the input-debug dump's dir
/// and timestamp conventions ([`crate::input_log`]). Also the target of the
/// `/debug log` runtime toggle ([`super::mouse::MouseScrollState`]).
pub(crate) fn default_log_path() -> PathBuf {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    echo_agent_tools::util::echo_agent_home::echo_agent_home()
        .join("logs")
        .join(format!("scroll-log-{ts}.jsonl"))
}
