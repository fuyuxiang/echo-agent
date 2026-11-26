//! Sweep-harness stage marks for the subagent spawn pipeline. Disabled by
//! default: the disabled path is a single atomic sink check and reads no clock.
//! `ECHO_AGENT_SUBAGENT_WATERFALL=1` writes to stderr; a `/path` value appends to
//! that file so the regression tier can parse its own marks back. Timestamps
//! are monotonic micros from a process epoch ([`now_us`]): wall clocks step
//! under NTP and skew segment math.
//!
//! Mark ids: a subagent's mark id is its request id, which equals both the
//! child session id and the Task tool's `task_id`.
//!
//! Deliberately NOT a `SubagentSpawnPhase` sink: marks need a monotonic
//! clock shared with out-of-process consumers (the harness's client events
//! and mock arrivals), while the analytics schema is a closed, wall-clock-free
//! set of per-spawn durations.
//!
//! `pub` for the sweep harness only.

use std::sync::LazyLock;
use std::time::Instant;

/// Stage names the regression tier and harness parse as exact strings. The
/// gate reads two segments — `SESSION_SPAWN`→`SESSION_UP` (sessboot) and
/// `SB_BUILDER_DONE`→`SB_AGENT_BUILT` (bridge); `MOCK_REQ` is the harness's own
/// mock-arrival mark. The fine-grained pipeline stages were dropped with the
/// renderer that consumed them.
pub mod stage {
    /// Child session construction started (sessboot segment start).
    pub const SESSION_SPAWN: &str = "session_spawn";
    /// Child session actor ready (sessboot segment end).
    pub const SESSION_UP: &str = "session_up";
    /// Agent builder returned (bridge segment start).
    pub const SB_BUILDER_DONE: &str = "sb_builder_done";
    /// Agent wired into the child session (bridge segment end).
    pub const SB_AGENT_BUILT: &str = "sb_agent_built";
    /// Harness-emitted: the child's chat request arrived at the mock server.
    pub const MOCK_REQ: &str = "mock_req";
}

pub const ENV: &str = "ECHO_AGENT_SUBAGENT_WATERFALL";
/// Line shape: `WATERFALL id=<id> stage=<stage> t_us=<micros>`.
pub const LINE_PREFIX: &str = "WATERFALL";
/// Harness burst-origin line: `WATERFALL-T0 n=<n> t_us=<micros>`.
pub const T0_LINE_PREFIX: &str = "WATERFALL-T0";

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Monotonic micros since the process epoch; the harness stamps its own
/// timeline with this so shell marks and client events share one clock.
pub fn now_us() -> u128 {
    EPOCH.elapsed().as_micros()
}

pub fn mark(id: &str, stage: &str) {
    mark_with_clock(id, stage, now_us);
}

/// Split from [`mark`] so a test can prove the disabled path never reads the
/// clock; `clock` yields `t_us` and runs only for a live sink.
fn mark_with_clock(id: &str, stage: &str, clock: impl FnOnce() -> u128) {
    enum Sink {
        Off,
        Stderr,
        File(std::sync::Mutex<std::fs::File>),
    }
    static SINK: std::sync::OnceLock<Sink> = std::sync::OnceLock::new();
    let sink = SINK.get_or_init(|| match std::env::var(ENV) {
        Err(_) => Sink::Off,
        Ok(v) if v.starts_with('/') => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&v)
            .map(|f| Sink::File(std::sync::Mutex::new(f)))
            .unwrap_or(Sink::Stderr),
        Ok(_) => Sink::Stderr,
    });
    // Gate first: the disabled path (normal operation) returns before touching
    // the clock; only a live sink pays for now_us().
    let file = match sink {
        Sink::Off => return,
        Sink::Stderr => None,
        Sink::File(f) => Some(f),
    };
    let t_us = clock();
    match file {
        None => eprintln!("{LINE_PREFIX} id={id} stage={stage} t_us={t_us}"),
        Some(f) => {
            use std::io::Write as _;
            if let Ok(mut f) = f.lock() {
                let _ = writeln!(f, "{LINE_PREFIX} id={id} stage={stage} t_us={t_us}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn disabled_sink_reads_no_clock() {
        // The lib test process never sets `ENV`, so the sink latches Off and the
        // clock closure must never run.
        let reads = Cell::new(0u32);
        mark_with_clock("swp-x", stage::SESSION_SPAWN, || {
            reads.set(reads.get() + 1);
            0
        });
        assert_eq!(reads.get(), 0, "disabled mark must not read the clock");
    }
}
