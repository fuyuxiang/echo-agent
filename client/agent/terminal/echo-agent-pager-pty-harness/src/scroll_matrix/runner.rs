//! The per-cell executor: spawn the primed pager, replay the gesture,
//! synchronize on the recorder, judge the invariants, classify the verdict.
//!
//! ## Blocking model
//!
//! A cell's body ([`run_cell_inner`]) is host-paced end to end — PTY drains,
//! `thread::sleep` gesture gaps, finalize polling — so [`run_cell`] runs it
//! on `spawn_blocking` (driving the async session spawns via
//! `Handle::block_on`, which requires a **multi-thread** runtime) and applies
//! the per-cell hard cap with `tokio::time::timeout` on the join handle.
//! That also converts setup panics (the session preambles assert with the
//! screen contents) into a `Fail` report instead of killing the whole sweep.
//! A capped cell's blocking task cannot be aborted mid-syscall: it is left
//! to unwind on its own bounded waits (its `Drop`s kill the pager child and
//! mock server), while the sweep moves on.
//!
//! ## Harness-side invariants
//!
//! [`InvariantId::Screen`] and [`InvariantId::Quiet`] are checked here (the
//! log alone can't see the viewport or repaints — `invariants.rs` panics on
//! them by contract): I-QUIET counts frames in a post-finalize watermark
//! window, I-SCREEN replays the per-stream `applied_total`s through a
//! bottom-clamped travel simulation and compares the topmost visible marker
//! against the session baseline.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::cells::{MatrixCell, Tier};
use super::invariants::{InvariantId, InvariantResult, check_log_invariant};
use super::log::{StreamGroup, group_streams, parse_jsonl, wait_for_finalize_count};
use super::report::{CellReport, CellStatus, InvariantReport, InvariantStatus};
use super::session::{
    SessionKind, WHEEL_COL, WHEEL_ROW, spawn_marker_session, topmost_visible_marker,
};

/// Transcript height for every cell: comfortably taller than the 50-row PTY
/// (the preamble's scrollable-baseline guard) with enough headroom that the
/// small gestures judged by I-SCREEN never clamp at the transcript top.
/// Deliberate travel clamping (floods can deliver hundreds of rows) is fine:
/// no log-side invariant reads the viewport.
const MARKER_COUNT: usize = 400;

/// Recorder-synchronization budget: every gesture table spans < 1.5s and a
/// stream finalizes 80ms after its last event, so 5s only trips on a real
/// wedge (or a stall so long the cell is unusable anyway).
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(5);

/// Post-finalize settle: consume the gesture-era PTY backlog so the quiet
/// window below counts only NEW frames (the harness parses chunks lazily in
/// `update`, not at arrival).
const PIPELINE_DRAIN: Duration = Duration::from_millis(300);

/// I-QUIET observation window after the drain + watermark reset.
const QUIET_WINDOW: Duration = Duration::from_millis(500);

/// I-QUIET allowance: a straggler cadence/finalize paint mid-pipeline may
/// land after the watermark; repaint CHURN (the A2 symptom) paints dozens.
const QUIET_MAX_FRAMES: u64 = 2;

/// Streaming-session teardown budget: the paced tail is ~7s at spawn time,
/// so the released turn completes well inside this.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-cell hard cap (spawn → verdict). The slowest legitimate cell — the
/// streaming preamble plus its post-gesture tail drain — finishes in ~20s.
const CELL_HARD_CAP: Duration = Duration::from_secs(60);

/// `tier` label for [`CellReport`].
fn tier_label(tier: Tier) -> &'static str {
    match tier {
        Tier::Curated => "curated",
        Tier::Full => "full",
    }
}

/// One SGR (DECSET 1006) wheel press report at the shared in-scrollback
/// position — 0-based [`WHEEL_ROW`]/[`WHEEL_COL`] encode 1-based on the wire
/// (same bytes as the pager e2e `sgr_mouse` helper).
fn sgr_wheel_report(button: u16) -> String {
    format!("\x1b[<{button};{};{}M", WHEEL_COL + 1, WHEEL_ROW + 1)
}

/// Run one matrix cell against `binary`, capturing the recorder JSONL to
/// `artifacts_dir/<cell_id>.jsonl` (kept for post-mortems). Never panics and
/// never hangs past [`CELL_HARD_CAP`]; every abnormality becomes a `Fail`
/// report with a phase note. Requires a multi-thread tokio runtime (see the
/// module docs).
pub async fn run_cell(cell: &MatrixCell, binary: &Path, artifacts_dir: &Path) -> CellReport {
    let started = Instant::now();
    let cell = *cell;
    let log_path = artifacts_dir.join(format!("{}.jsonl", cell.id));

    let outcome = match std::fs::create_dir_all(artifacts_dir)
        .with_context(|| format!("create artifacts dir {}", artifacts_dir.display()))
    {
        Err(err) => Err(format!("artifacts setup: {err:#}")),
        Ok(()) => {
            let handle = tokio::runtime::Handle::current();
            let (binary, inner_log) = (binary.to_path_buf(), log_path.clone());
            let task = tokio::task::spawn_blocking(move || {
                handle.block_on(run_cell_inner(cell, &binary, &inner_log))
            });
            match tokio::time::timeout(CELL_HARD_CAP, task).await {
                Err(_) => Err(format!(
                    "hard cap: cell still running after {CELL_HARD_CAP:?} (phase unknown; \
                     the cell task is left to unwind on its own bounded waits)"
                )),
                Ok(Err(join_err)) => Err(format!("panic: {}", panic_message(join_err))),
                Ok(Ok(Err(err))) => Err(format!("{err:#}")),
                Ok(Ok(Ok(run))) => Ok(run),
            }
        }
    };

    let (status, invariants, streams, note) = match outcome {
        Ok(run) => {
            let (status, invariants) = classify(&run.outcomes, cell.xfail);
            (status, invariants, run.streams, None)
        }
        Err(note) => (CellStatus::Fail, Vec::new(), 0, Some(note)),
    };
    CellReport {
        cell_id: cell.id.to_owned(),
        tier: tier_label(cell.tier).to_owned(),
        status,
        invariants,
        log_path: log_path.display().to_string(),
        streams,
        duration_ms: started.elapsed().as_millis() as u64,
        note,
    }
}

fn panic_message(err: tokio::task::JoinError) -> String {
    match err.try_into_panic() {
        Ok(payload) => payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned()),
        Err(err) => format!("cell task failed without panicking: {err}"),
    }
}

/// Everything [`run_cell`] needs from a completed (non-aborted) cell body.
struct CellRun {
    outcomes: Vec<(InvariantId, InvariantResult)>,
    streams: usize,
}

async fn run_cell_inner(cell: MatrixCell, binary: &Path, log_path: &Path) -> Result<CellRun> {
    // Stale-capture guard: the recorder opens its file lazily on the first
    // record, so a leftover capture from a previous run would satisfy the
    // finalize wait with the OLD gesture's records.
    if log_path.exists() {
        std::fs::remove_file(log_path)
            .with_context(|| format!("setup: remove stale capture {}", log_path.display()))?;
    }
    let log_value = log_path
        .to_str()
        .context("setup: artifacts path is not UTF-8")?;
    let mut env: Vec<(&str, &str)> = cell.env.to_vec();
    env.push(("ECHO_AGENT_SCROLL_LOG", log_value));

    let (mut harness, content, baseline, streaming_turn) =
        spawn_marker_session(binary, cell.session, MARKER_COUNT, &env).await;

    // Replay the gesture table: sleep each step's pre-delay (host-side lower
    // bound — jitter only stretches gaps), then emit its report. Port of the
    // pager e2e `send_wheel_sequence` loop onto `WheelStep`.
    for step in cell.gesture.steps(cell.expected.ept) {
        if step.pre_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(step.pre_delay_ms));
        }
        harness
            .inject_keys(sgr_wheel_report(step.button).as_bytes())
            .context("gesture: inject wheel report")?;
    }

    wait_for_finalize_count(log_path, cell.gesture.expected_streams(), FINALIZE_TIMEOUT)
        .context("gesture: finalize wait")?;

    // Drain the gesture-era backlog, then watermark → the quiet window
    // counts only post-finalize frames; the marker read afterwards sees the
    // fully painted final viewport (I-SCREEN's input).
    harness.update(PIPELINE_DRAIN);
    harness.reset_timing();
    harness.update(QUIET_WINDOW);
    let quiet_frames = harness.frame_count();
    let marker_after = topmost_visible_marker(&harness);

    if let Some(mut streaming_turn) = streaming_turn {
        streaming_turn.release();
        let deadline = Instant::now() + COMPLETION_TIMEOUT;
        while harness.contains_text("Responding") {
            if Instant::now() >= deadline {
                bail!("teardown: turn never completed after the expectation release");
            }
            harness.update(Duration::from_millis(200));
        }
        tokio::time::timeout(COMPLETION_TIMEOUT, streaming_turn.wait_satisfied())
            .await
            .context("teardown: streaming expectation was not satisfied")?;
    }
    harness.quit().context("teardown: quit pager")?;
    drop(content);

    // The pager exited (recorder flushed + closed), so the capture is
    // complete and torn-tail-free by construction.
    let records = parse_jsonl(log_path).context("verdict: parse capture")?;
    let groups = group_streams(&records).context("verdict: group streams")?;

    let outcomes = cell
        .invariants
        .iter()
        .map(|&id| {
            let result = if id.is_log_side() {
                check_log_invariant(id, &cell.expected, &groups)
            } else {
                match id {
                    InvariantId::Screen => {
                        check_screen(cell.session, baseline, marker_after, &groups)
                    }
                    InvariantId::Quiet => check_quiet(quiet_frames),
                    _ => unreachable!("is_log_side covers every other id"),
                }
            };
            (id, result)
        })
        .collect();
    Ok(CellRun {
        outcomes,
        streams: groups.len(),
    })
}

/// I-QUIET: no repaint churn after the last finalize — at most
/// [`QUIET_MAX_FRAMES`] frames land in the watermark window.
fn check_quiet(quiet_frames: u64) -> InvariantResult {
    if quiet_frames > QUIET_MAX_FRAMES {
        return InvariantResult::Violated {
            detail: format!(
                "{quiet_frames} frames painted in the {QUIET_WINDOW:?} post-finalize window \
                 (> {QUIET_MAX_FRAMES}) — repaint churn after the gesture ended"
            ),
        };
    }
    InvariantResult::Pass
}

/// Net signed lines a stream delivered: its last flush-bearing record's
/// cumulative `applied_total` (finalize when present, else the last flush of
/// a trailing in-flight stream).
fn stream_applied(group: &StreamGroup<'_>) -> i64 {
    group
        .flush_bearing()
        .last()
        .map_or(0, |record| record.applied_total)
}

/// Replay per-stream deliveries through the viewport's clamps: `0` is the
/// bottom pin (down-deliveries there don't move — G7's point), and the
/// return is clamped to `-baseline` because travel above marker 0 pins the
/// topmost visible marker at 0.
fn simulate_clamped_travel(baseline: usize, applied: impl IntoIterator<Item = i64>) -> i64 {
    let mut pos: i64 = 0;
    for delta in applied {
        pos = (pos + delta).min(0);
    }
    pos.max(-(baseline as i64))
}

/// I-SCREEN: the viewport visibly moved/clamped exactly as the recorder
/// says it should have — topmost-marker delta equals the bottom-clamped
/// replay of the per-stream `applied_total`s (up is negative, matching the
/// producer's `ScrollDirection` sign and the marker index direction).
fn check_screen(
    session: SessionKind,
    baseline: usize,
    marker_after: Option<usize>,
    groups: &[StreamGroup<'_>],
) -> InvariantResult {
    if session == SessionKind::Streaming {
        // The live bottom keeps growing mid-stream, so "marker delta ==
        // applied" has no stable frame of reference — a cell-table bug.
        return InvariantResult::Violated {
            detail: "I-SCREEN attached to a streaming session (no stable baseline)".to_owned(),
        };
    }
    let Some(after) = marker_after else {
        return InvariantResult::Violated {
            detail: "no marker visible after the gesture".to_owned(),
        };
    };
    let expected =
        baseline as i64 + simulate_clamped_travel(baseline, groups.iter().map(stream_applied));
    if after as i64 != expected {
        return InvariantResult::Violated {
            detail: format!(
                "topmost marker {baseline} -> {after} after the gesture, but the clamped \
                 replay of the streams' applied totals lands at {expected}"
            ),
        };
    }
    InvariantResult::Pass
}

/// XPASS row detail: the actionable half of the xfail contract.
const XPASS_DETAIL: &str = "expected to violate (xfail) but PASSED — the pinned bug got fixed \
                            or the cell rotted; promote the invariant out of the xfail set";

/// Classify evaluated invariants into per-row statuses and the cell verdict.
/// Precedence: any `Fail` (non-xfail violation) fails the cell; else any
/// `XPass` fails it (fixed/rotted xfail must be promoted, not absorbed);
/// else any `XFail` marks the expected failure; else `Pass`.
fn classify(
    outcomes: &[(InvariantId, InvariantResult)],
    xfail: &[InvariantId],
) -> (CellStatus, Vec<InvariantReport>) {
    let rows: Vec<InvariantReport> = outcomes
        .iter()
        .map(|(id, result)| {
            let expected_to_fail = xfail.contains(id);
            let (status, detail) = match (result, expected_to_fail) {
                (InvariantResult::Pass, false) => (InvariantStatus::Pass, None),
                (InvariantResult::Pass, true) => {
                    (InvariantStatus::XPass, Some(XPASS_DETAIL.to_owned()))
                }
                (InvariantResult::Violated { detail }, false) => {
                    (InvariantStatus::Fail, Some(detail.clone()))
                }
                (InvariantResult::Violated { detail }, true) => {
                    (InvariantStatus::XFail, Some(detail.clone()))
                }
            };
            InvariantReport {
                id: id.as_str().to_owned(),
                status,
                detail,
            }
        })
        .collect();

    let has = |status: InvariantStatus| rows.iter().any(|row| row.status == status);
    let status = if has(InvariantStatus::Fail) {
        CellStatus::Fail
    } else if has(InvariantStatus::XPass) {
        CellStatus::XPass
    } else if has(InvariantStatus::XFail) {
        CellStatus::XFail
    } else {
        CellStatus::Pass
    };
    (status, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violated(detail: &str) -> InvariantResult {
        InvariantResult::Violated {
            detail: detail.to_owned(),
        }
    }

    /// The wire bytes of the shared wheel position, pinned to the
    /// `40;12` encoding documented on `WHEEL_ROW`/`WHEEL_COL`.
    #[test]
    fn sgr_report_encodes_one_based_wire_coords() {
        assert_eq!(sgr_wheel_report(64), "\x1b[<64;40;12M");
        assert_eq!(sgr_wheel_report(65), "\x1b[<65;40;12M");
    }

    #[test]
    fn clamped_travel_models_bottom_pin_and_top_clamp() {
        // G7's shape: down-deliveries at the pin don't move, the up tail does.
        assert_eq!(simulate_clamped_travel(300, [10, -7]), -7);
        // Up then partially back down: net movement, no clamp involved.
        assert_eq!(simulate_clamped_travel(300, [-5, 3]), -2);
        // Pure down at the pin stays put.
        assert_eq!(simulate_clamped_travel(300, [10]), 0);
        // Down past the pin then up: the overshoot must not bank as credit.
        assert_eq!(simulate_clamped_travel(300, [25, -4]), -4);
        // Travel beyond the transcript top pins the topmost marker at 0.
        assert_eq!(simulate_clamped_travel(30, [-500]), -30);
    }

    #[test]
    fn quiet_allows_the_straggler_allowance_only() {
        assert!(check_quiet(0).is_pass());
        assert!(check_quiet(QUIET_MAX_FRAMES).is_pass());
        let result = check_quiet(QUIET_MAX_FRAMES + 1);
        assert!(
            matches!(result, InvariantResult::Violated { ref detail } if detail.contains("churn"))
        );
    }

    #[test]
    fn screen_rejects_streaming_sessions_and_marker_loss() {
        let streaming = check_screen(SessionKind::Streaming, 100, Some(100), &[]);
        assert!(
            matches!(streaming, InvariantResult::Violated { ref detail } if detail.contains("streaming"))
        );
        let lost = check_screen(SessionKind::BottomPinned, 100, None, &[]);
        assert!(
            matches!(lost, InvariantResult::Violated { ref detail } if detail.contains("no marker"))
        );
        // Empty capture ⇒ no movement expected; a matching marker passes.
        assert!(check_screen(SessionKind::BottomPinned, 100, Some(100), &[]).is_pass());
        let moved = check_screen(SessionKind::BottomPinned, 100, Some(97), &[]);
        assert!(matches!(moved, InvariantResult::Violated { .. }));
    }

    #[test]
    fn classify_precedence_fail_over_xpass_over_xfail_over_pass() {
        use InvariantId::{Cap, NoDrop, Ord, SmoothCoast};

        // All pass, nothing xfailed → Pass.
        let (status, rows) = classify(&[(Ord, InvariantResult::Pass)], &[]);
        assert_eq!(status, CellStatus::Pass);
        assert_eq!(rows[0].status, InvariantStatus::Pass);
        assert_eq!(rows[0].id, "I-ORD");

        // The declared bug violates, everything else passes → XFail.
        let jerk = [
            (Ord, InvariantResult::Pass),
            (SmoothCoast, violated("coast")),
            (NoDrop, violated("dropped 74")),
        ];
        let (status, rows) = classify(&jerk, &[SmoothCoast, NoDrop]);
        assert_eq!(status, CellStatus::XFail);
        assert_eq!(rows[1].status, InvariantStatus::XFail);
        assert_eq!(rows[1].detail.as_deref(), Some("coast"));

        // One xfail row passing flips the cell to XPass (fixed-bug tripwire)…
        let half_fixed = [
            (SmoothCoast, InvariantResult::Pass),
            (NoDrop, violated("dropped 74")),
        ];
        let (status, rows) = classify(&half_fixed, &[SmoothCoast, NoDrop]);
        assert_eq!(status, CellStatus::XPass);
        assert!(rows[0].detail.as_deref().unwrap().contains("promote"));

        // …but any non-xfail violation dominates everything.
        let broken = [
            (Cap, violated("flushed 40 exceeds cap 25")),
            (SmoothCoast, InvariantResult::Pass),
        ];
        let (status, _) = classify(&broken, &[SmoothCoast]);
        assert_eq!(status, CellStatus::Fail);
    }

    #[test]
    fn tier_labels_match_the_report_vocabulary() {
        assert_eq!(tier_label(Tier::Curated), "curated");
        assert_eq!(tier_label(Tier::Full), "full");
    }
}
