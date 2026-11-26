//! Curated scroll-matrix tier: one test per curated cell, exact-filterable
//! by cell id (`cargo test --test scroll_matrix_curated c5_tmux_g9a -- --exact`).
//!
//! Each test drives `scroll_matrix::run_cell` against the real pager binary
//! (PAGER_BINARY / local debug build — same contract as
//! `scroll_correctness_echo-agent-pty.rs`, and like it NOT `#[ignore]`d). Cells are
//! host-paced PTY sessions, so a process-wide lock serializes them: the
//! default in-process test parallelism would stretch gesture gaps and stack
//! eight pagers onto one machine.
//!
//! Artifacts (recorder captures + per-cell report.json rows) land under
//! `$TMPDIR/scroll-matrix-curated/` and are kept for post-mortems.

use std::path::PathBuf;

use echo_agent_pager_pty_harness::pager_binary;
use echo_agent_pager_pty_harness::scroll_matrix::{
    CellReport, CellStatus, curated, run_cell, summary_table,
};

/// Serializes cells across the in-process test threads (tokio mutex: held
/// across awaits; no poisoning, so one failed cell doesn't cascade).
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Per-cell captures land here. Prefer Bazel's `TEST_TMPDIR` (unique and
/// isolated per test action) over the shared system temp dir: this target is
/// `tags = ["local"]`, so concurrent executions on a CI host would otherwise
/// share a stable `/tmp/scroll-matrix-curated/<cell_id>.jsonl` path — a
/// second run's stale-capture `remove_file` (and its pager's `ECHO_AGENT_SCROLL_LOG`
/// writer) then corrupts the first run's in-flight capture, surfacing as a
/// `parse capture: No such file or directory` or a mid-record parse error.
/// Falls back to the system temp dir for plain `cargo test`.
fn artifacts_dir() -> PathBuf {
    std::env::var_os("TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("scroll-matrix-curated")
}

/// Run one curated cell by id and return its report (panics on unknown ids
/// so a renamed cell can't leave a vacuous test behind).
async fn run_curated_cell(cell_id: &str) -> CellReport {
    let cell = curated()
        .find(|cell| cell.id == cell_id)
        .unwrap_or_else(|| panic!("{cell_id} is not a curated cell — update this test file"));
    let binary = pager_binary().expect("resolve pager binary");
    let _serial = SERIAL.lock().await;
    let report = run_cell(cell, &binary, &artifacts_dir()).await;
    eprintln!(
        "{}",
        summary_table(std::slice::from_ref(&report)).trim_end()
    );
    report
}

/// Assert a normal (no-xfail) curated cell passes outright.
async fn assert_cell_passes(cell_id: &str) {
    let report = run_curated_cell(cell_id).await;
    assert_eq!(
        report.status,
        CellStatus::Pass,
        "{cell_id} did not pass — capture kept at {}\n{}",
        report.log_path,
        serde_json::to_string_pretty(&report).expect("report json"),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c1_auto_g3_flood_speed100() {
    assert_cell_passes("c1_auto_g3_flood_speed100").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c2_auto_g3_flood_speed100() {
    assert_cell_passes("c2_auto_g3_flood_speed100").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c3_wheel_lines1_g1() {
    assert_cell_passes("c3_wheel_lines1_g1").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c4_auto_g10_ambiguous() {
    assert_cell_passes("c4_auto_g10_ambiguous").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c5_tmux_g9a() {
    assert_cell_passes("c5_tmux_g9a").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c5_tmux_g9b() {
    assert_cell_passes("c5_tmux_g9b").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c1_auto_g8_midstream() {
    assert_cell_passes("c1_auto_g8_midstream").await;
}

/// The formerly-declared bug: the finalize-decel fix landed, so the G4 jerk
/// cell passes outright — I-SMOOTH-COAST (post-input motion at most one
/// tapered cap) and I-NO-DROP (finalize discards nothing) moved from the
/// xfail set to ordinary pass rows. The cell id keeps its historical name
/// for artifact continuity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c1_auto_g4_jerk_xfail() {
    assert_cell_passes("c1_auto_g4_jerk_xfail").await;
}

/// Tripwire: every curated cell has a test above (a row added to the
/// curated tier without a matching test would otherwise silently skip CI).
#[test]
fn curated_cells_all_have_a_test() {
    let covered = [
        "c1_auto_g3_flood_speed100",
        "c2_auto_g3_flood_speed100",
        "c3_wheel_lines1_g1",
        "c4_auto_g10_ambiguous",
        "c5_tmux_g9a",
        "c5_tmux_g9b",
        "c1_auto_g8_midstream",
        "c1_auto_g4_jerk_xfail",
    ];
    let curated_ids: Vec<&str> = curated().map(|cell| cell.id).collect();
    assert_eq!(
        curated_ids, covered,
        "curated tier changed — add/remove the matching #[tokio::test] fns in this file"
    );
}
