//! A requested quit exits within the exit timeout even when teardown hangs.
//! `ECHO_AGENT_TEST_HOLD_TEARDOWN_SECS` supplies the hang; a real `SessionEnd`
//! hook cannot hold teardown past `SESSION_FLUSH_GRACE`.
//!
//! ```bash
//! cargo test -p echo-agent-pager-pty-harness --test exit_timeout -- --ignored
//! ```

use std::time::Duration;

use anyhow::{Context, Result};
use echo_agent_pager_pty_harness::{
    ContentController, PtyExitPoll, PtyHarness, keys, pager_binary,
};

const ROWS: u16 = 50;
const COLS: u16 = 120;
const ACK: &str = "ACKSENTINEL";
const CANARY: &str = "EXITTIMEOUTCANARY9";
const TIMEOUT_SECS: &str = "2";
const HOLD_SECS: &str = "120";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // opt-in: spawns the real pager binary in a PTY (CI runs with --ignored)
async fn double_ctrl_c_exits_within_deadline_when_teardown_hangs() -> Result<()> {
    let (mut pager, _project) = spawn_pager_with_teardown_hold().await?;

    // First Ctrl+C arms the quit confirmation, the second confirms.
    pager.inject_keys(keys::CTRL_C)?;
    pager.update(Duration::from_millis(250));
    pager.inject_keys(keys::CTRL_C)?;

    assert_forced_exit(&mut pager, 0)
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // opt-in: spawns the real pager binary in a PTY (CI runs with --ignored)
async fn single_sighup_exits_within_deadline_when_teardown_hangs() -> Result<()> {
    let (mut pager, _project) = spawn_pager_with_teardown_hold().await?;

    // One real SIGHUP; an orphan never receives a second signal.
    pager.send_signal(libc::SIGHUP)?;

    assert_forced_exit(&mut pager, 129)
}

/// With teardown held for [`HOLD_SECS`], only the exit timeout can end the
/// process inside the wait budget.
fn assert_forced_exit(pager: &mut PtyHarness, exit_code: u32) -> Result<()> {
    pager.update(Duration::from_secs(10));
    let exit = pager.wait_exit_code(Duration::from_secs(20))?;
    assert_eq!(exit, PtyExitPoll::Exited(exit_code));
    Ok(())
}

/// Spawn with a short exit timeout and a long teardown hold, then submit a
/// prompt and let the turn settle so the quit exercises a started session.
async fn spawn_pager_with_teardown_hold() -> Result<(PtyHarness, tempfile::TempDir)> {
    let content = ContentController::start()
        .await
        .context("start mock server")?;
    content.set_response(format!("{ACK} acknowledged."));

    let project = tempfile::tempdir()?;
    std::fs::create_dir_all(project.path().join(".git"))?;

    let binary = pager_binary()?;
    let mut pager = PtyHarness::spawn_with_content_env_in_dir(
        &binary,
        ROWS,
        COLS,
        &content,
        &[],
        &[
            ("ECHO_AGENT_EXIT_TIMEOUT_SECS", TIMEOUT_SECS),
            ("ECHO_AGENT_TEST_HOLD_TEARDOWN_SECS", HOLD_SECS),
        ],
        Some(project.path()),
    )?;

    pager.wait_for_text("Quit", Duration::from_secs(20))?;
    pager.inject_keys(CANARY.as_bytes())?;
    pager.inject_keys(keys::ENTER)?;
    pager.wait_for_text(ACK, Duration::from_secs(30))?;
    pager.update(Duration::from_millis(1000));
    Ok((pager, project))
}
