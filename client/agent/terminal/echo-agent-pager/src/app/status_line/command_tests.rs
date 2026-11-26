use super::*;

/// Any plausible row: only the test that reads `$COLUMNS` cares about the size.
const ROW: RowSize = RowSize {
    cols: 80,
    lines: 24,
};

async fn run_row(command: &str) -> String {
    row_text(run_status_command(command, &ctx(), ROW, COMMAND_TIMEOUT).await)
}

/// What a state-triggered run would paint, which is what these cases assert.
fn row_text(outcome: RunOutcome) -> String {
    match outcome {
        RunOutcome::Output(line) => line,
        RunOutcome::Failed { text, .. } => text,
    }
}

fn ctx() -> StatusLineContext {
    crate::app::status_line::test_context("/tmp")
}

#[tokio::test]
async fn script_is_handed_the_payload_on_stdin_and_the_terminal_size() {
    let script = "cat; printf '%s %s' \"$COLUMNS\" \"$LINES\"";
    let row = run_command(
        script,
        &ctx(),
        RowSize {
            cols: 137,
            lines: 24,
        },
        COMMAND_TIMEOUT,
    )
    .await
    .unwrap();

    assert!(row.contains("\"cwd\":\"/tmp\""), "got {row}");
    assert!(row.ends_with("137 24"), "got {row}");
}

#[tokio::test]
async fn payload_past_a_pipe_buffer_survives_a_script_that_writes_first() {
    let mut ctx = ctx();
    ctx.session_name = Some("n".repeat(128 * 1024));
    // Past the 64 KiB Linux pipe buffer in both directions.
    let script = "head -c 70000 /dev/zero | tr '\\0' x; cat >/dev/null; printf done";

    let row = tokio::time::timeout(
        Duration::from_secs(5),
        run_command(script, &ctx, ROW, COMMAND_TIMEOUT),
    )
    .await
    .expect("the payload write blocked against the script's own stdout");

    assert!(row.is_ok(), "{row:?}");
}

#[tokio::test]
async fn non_zero_exit_reports_the_code_only_when_it_printed_nothing() {
    assert_eq!(run_row("exit 3").await, "[status line: exit 3]");
    assert_eq!(run_row("printf ok; exit 3").await, "ok");
}

#[tokio::test]
async fn runaway_output_is_capped_rather_than_waiting_out_the_deadline() {
    let started = Instant::now();
    let row = run_command("yes hello", &ctx(), ROW, COMMAND_TIMEOUT)
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let lines: Vec<&str> = row.split('\n').collect();

    assert!(elapsed < COMMAND_TIMEOUT / 2, "took {elapsed:?}");
    assert_eq!(lines.len(), MAX_STATUS_LINE_LINES as usize);
    // A `<=` bound on the length is also satisfied by the empty string.
    assert!(lines.iter().all(|line| *line == "hello"), "got {row:?}");
}

/// Past the 64 KiB the log keeps and the 64 KiB the pipe buffers, so the script
/// is still writing when the log has all it wants.
const STDERR_PAST_THE_CAP: &str = "printf '%300000s' '' >&2";

#[tokio::test]
async fn stderr_past_the_cap_neither_kills_the_script_nor_stalls_it() {
    let row = run_row(&format!("{STDERR_PAST_THE_CAP}; printf ok")).await;

    assert_eq!(row, "ok", "stderr is drained and never painted");
}

#[tokio::test]
async fn script_that_closes_stdout_early_still_answers() {
    // stdout reaches EOF while the script is still writing to stderr, so the
    // drain has to keep running while we wait for the script to exit.
    let row = run_row(&format!("printf ok; exec 1>&-; {STDERR_PAST_THE_CAP}")).await;

    assert_eq!(row, "ok");
}

/// The wrapper backgrounds a grandchild that touches a marker 0.5s in, long
/// after the run should have torn the group down. Returns the row and whether
/// the grandchild lived to touch it.
async fn no_grandchild_survives(case: &str, head: &str, timeout: Duration) -> (String, bool) {
    grandchild_outcome(
        case,
        |marker| format!("{head} & (sleep 0.5; touch {marker}) & wait"),
        timeout,
    )
    .await
}

async fn grandchild_outcome(
    case: &str,
    script: impl FnOnce(&str) -> String,
    timeout: Duration,
) -> (String, bool) {
    // `case` rather than anything derived from the script: the callers run
    // concurrently, and a shared path lets one clear the other's marker.
    let marker =
        std::env::temp_dir().join(format!("status_line-group-{}-{case}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let script = script(&marker.display().to_string());

    let row = row_text(run_status_command(&script, &ctx(), ROW, timeout).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let survived = marker.exists();
    let _ = std::fs::remove_file(&marker);
    (row, survived)
}

#[tokio::test]
async fn timeout_kills_the_whole_process_group() {
    let (row, survived) =
        no_grandchild_survives("timeout", "sleep 300", Duration::from_millis(150)).await;

    assert_eq!(row, "[status line: timed out]");
    assert!(!survived, "a backgrounded grandchild outlived the timeout");
}

#[tokio::test]
async fn script_that_exits_cleanly_still_loses_what_it_backgrounded() {
    // Redirected, or the background job holds stdout open and the read waits
    // for it: the run would then end on the timeout, not on a clean exit.
    let (row, survived) = grandchild_outcome(
        "clean-exit",
        |marker| format!("(sleep 0.5; touch {marker}) >/dev/null 2>&1 & printf row"),
        COMMAND_TIMEOUT,
    )
    .await;

    assert_eq!(row, "row");
    assert!(!survived, "a grandchild outlived a script that exited 0");
}

#[tokio::test]
async fn background_job_holding_stdout_does_not_hold_the_row() {
    // No redirect, so the grandchild inherits stdout and the pipe stays open
    // for five seconds after the shell exits. Reading to EOF would wait for it.
    let started = Instant::now();
    let row =
        row_text(run_status_command("sleep 5 & printf row", &ctx(), ROW, COMMAND_TIMEOUT).await);

    assert_eq!(row, "row");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the row waited {:?} for a pipe the script had already finished with",
        started.elapsed()
    );
}

#[tokio::test]
async fn capped_runaway_still_has_its_process_group_killed() {
    // The cap returns without a status, which is what leaves the guard armed.
    let (_, survived) = no_grandchild_survives("cap", "yes hello", COMMAND_TIMEOUT).await;

    assert!(!survived, "a grandchild outlived the capped run");
}
