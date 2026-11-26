//! E2E: a submitted prompt is durably recorded in the per-CWD
//! `prompt_history.jsonl` and survives quitting the TUI — via a fast double
//! Ctrl+C (the reported repro, recalled after a `--continue` resume) and via a
//! real OS SIGINT routed through the same graceful quit.
//!
//! Drives the real pager binary through a PTY against the shared mock
//! inference server (isolated `$HOME`), exercising the full
//! pager -> shell -> queue_input -> append path plus the graceful-quit teardown.
//!
//! Coverage note: both paths wait for the turn to land before quitting, so
//! `queue_input` (and its now-awaited append) has already run. This is an
//! end-to-end durability + recall check, not a probe of the old detached-append
//! race — that race is closed structurally by awaiting the append in
//! `queue_input` and is covered by the `prompt_history` unit test. The
//! deterministic regression catch here is the SIGINT path exiting 0 (pre-fix it
//! was `process::exit(130)`).
//!
//! ```bash
//! cargo test -p echo-agent-pager-pty-harness --test prompt_history_durable_quit \
//!   -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use echo_agent_pager_pty_harness::{
    ContentController, PtyExitPoll, PtyHarness, keys, pager_binary,
};

const ROWS: u16 = 50;
const COLS: u16 = 120;
const CANARY: &str = "REGRESSIONCANARY42";
#[cfg(unix)]
const SIGINT_CANARY: &str = "SIGINTCANARY7";
const ACK: &str = "ACKSENTINEL";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // opt-in: spawns the real pager binary in a PTY (CI runs with --ignored)
async fn prompt_history_durable_after_double_ctrl_c_and_recallable_on_resume() {
    run().await.expect("prompt-history durable-quit e2e");
}

/// A real OS SIGINT (not an injected Ctrl+C key byte) must route through the
/// same graceful quit: the prompt stays durable and the process exits 0.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // opt-in: spawns the real pager binary in a PTY (CI runs with --ignored)
async fn prompt_history_durable_after_real_sigint_graceful_quit() {
    run_sigint().await.expect("sigint graceful-quit e2e");
}

async fn run() -> Result<()> {
    let content = ContentController::start()
        .await
        .context("start mock server")?;
    content.set_response(format!("{ACK} acknowledged."));

    let project = tempfile::tempdir().context("project dir")?;
    std::fs::create_dir_all(project.path().join(".git")).context("create .git")?;

    let binary = pager_binary().context("resolve pager binary")?;

    // 1) Submit a prompt and let the turn settle (proves the shell reached
    //    queue_input, where the append happens), then quit with double Ctrl+C.
    let mut first = submit_and_settle(&binary, &content, project.path(), CANARY)
        .context("submit canary in first pager")?;

    // Double Ctrl+C: first arms the quit confirmation on the empty prompt, the
    // second confirms -> same graceful Action::Quit as `/exit`.
    let pre = first.raw_output().len();
    first.inject_keys(keys::CTRL_C).context("ctrl-c arm")?;
    first.update(Duration::from_millis(250));
    first.inject_keys(keys::CTRL_C).context("ctrl-c confirm")?;

    // Drain output until the child exits so the post-`pre` suffix holds the full
    // graceful teardown (incl. the show-cursor restore) for the assertions below.
    first.update(Duration::from_secs(10));

    let exit = first
        .wait_exit_code(Duration::from_secs(10))
        .context("wait for double-Ctrl+C exit")?;
    assert_eq!(
        exit,
        PtyExitPoll::Exited(0),
        "double Ctrl+C should exit via the graceful quit (exit 0), got {exit:?}"
    );
    assert!(
        terminal_restored(&first, pre),
        "terminal not restored (no show-cursor after quit) on the double-Ctrl+C path"
    );
    drop(first);

    // 2) Durability: the prompt must be on disk after the quit.
    assert_prompt_durable(content.home(), CANARY)?;

    // 3) Recall across restart: `--continue` resumes the session and replays it.
    let mut resumed = PtyHarness::spawn_with_content_in_dir(
        &binary,
        ROWS,
        COLS,
        &content,
        &["--continue"],
        Some(project.path()),
    )
    .context("spawn resumed pager")?;
    resumed
        .wait_for_text(CANARY, Duration::from_secs(20))
        .context("resumed session replayed the prior prompt")?;
    assert!(
        !resumed.contains_text("panicked"),
        "pager panicked on resume:\n{}",
        resumed.screen_contents()
    );

    // Up-arrow opens the history overlay; smoke-check it doesn't crash and the
    // recalled prompt stays reachable.
    resumed.inject_keys(keys::UP).context("press Up")?;
    resumed.update(Duration::from_millis(500));
    assert!(
        resumed.contains_text(CANARY),
        "canary not reachable after Up:\n{}",
        resumed.screen_contents()
    );
    assert!(
        !resumed.contains_text("panicked"),
        "pager panicked after Up:\n{}",
        resumed.screen_contents()
    );

    resumed.quit().context("quit resumed pager")?;
    Ok(())
}

/// Real-SIGINT variant of [`run`]: deliver an OS signal to the pager child and
/// assert the same graceful-quit outcome (durable prompt, clean exit).
#[cfg(unix)]
async fn run_sigint() -> Result<()> {
    let content = ContentController::start()
        .await
        .context("start mock server")?;
    content.set_response(format!("{ACK} acknowledged."));

    let project = tempfile::tempdir().context("project dir")?;
    std::fs::create_dir_all(project.path().join(".git")).context("create .git")?;

    let binary = pager_binary().context("resolve pager binary")?;

    let mut first = submit_and_settle(&binary, &content, project.path(), SIGINT_CANARY)
        .context("submit canary before SIGINT")?;

    // A real SIGINT, not an injected 0x03 key byte (raw mode delivers that as a
    // key event — the double-Ctrl+C path above), drives the OS-signal path.
    let pre = first.raw_output().len();
    first.send_signal(libc::SIGINT).context("send SIGINT")?;

    // Drain output until the child exits so the post-`pre` suffix holds the full
    // graceful teardown (incl. the show-cursor restore) for the assertions below.
    first.update(Duration::from_secs(10));

    // Pre-fix the SIGINT handler called std::process::exit(130); routing it
    // through the graceful quit exits 0 — the deterministic Part-B regression catch.
    let exit = first
        .wait_exit_code(Duration::from_secs(10))
        .context("wait for SIGINT exit")?;
    assert_eq!(
        exit,
        PtyExitPoll::Exited(0),
        "real SIGINT should route through the graceful quit (exit 0), got {exit:?}"
    );
    assert!(
        terminal_restored(&first, pre),
        "terminal not restored (no show-cursor after SIGINT) on the SIGINT path"
    );
    drop(first);

    // Durability: the prompt must be on disk despite the signal-driven quit.
    assert_prompt_durable(content.home(), SIGINT_CANARY)?;
    Ok(())
}

/// Spawn the pager in `project`, submit `canary`, then wait for the turn to
/// render + settle to idle. Waiting past `queue_input` is deliberate: it
/// guarantees the prompt reached the shell (so the durability assertion is
/// meaningful), at the cost of not reproducing the sub-millisecond
/// detached-append race (closed by awaiting the append; see the unit test).
fn submit_and_settle(
    binary: &Path,
    content: &ContentController,
    project: &Path,
    canary: &str,
) -> Result<PtyHarness> {
    let mut pager =
        PtyHarness::spawn_with_content_in_dir(binary, ROWS, COLS, content, &[], Some(project))
            .context("spawn pager")?;
    pager
        .wait_for_text("Quit", Duration::from_secs(20))
        .context("welcome screen")?;
    pager
        .inject_keys(canary.as_bytes())
        .context("type canary")?;
    pager.inject_keys(keys::ENTER).context("submit prompt")?;
    pager
        .wait_for_text(ACK, Duration::from_secs(30))
        .context("turn response rendered")?;
    pager.update(Duration::from_millis(1000)); // let the short turn finish (idle)
    Ok(pager)
}

/// Whether the pager emitted the show-cursor restore (`ESC [ ?25h`) after byte
/// offset `since`. Scanning only the post-quit suffix avoids matching the
/// show-cursor that normal rendering emits mid-session.
fn terminal_restored(h: &PtyHarness, since: usize) -> bool {
    const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
    let raw = h.raw_output();
    raw[since.min(raw.len())..]
        .windows(SHOW_CURSOR.len())
        .any(|w| w == SHOW_CURSOR)
}

/// Assert the per-CWD `prompt_history.jsonl` durably recorded `canary`.
fn assert_prompt_durable(home: &Path, canary: &str) -> Result<()> {
    let hist = find_prompt_history(home).context("locate prompt_history.jsonl")?;
    let body =
        std::fs::read_to_string(&hist).with_context(|| format!("read {}", hist.display()))?;
    eprintln!("[e2e] prompt_history.jsonl @ {}:\n{body}", hist.display());
    assert!(
        body.contains(canary),
        "prompt_history.jsonl is missing the submitted prompt after the quit:\n{body}"
    );
    Ok(())
}

/// The per-CWD history file lives at `<home>/.echo-agent/sessions/<enc-cwd>/prompt_history.jsonl`.
fn find_prompt_history(home: &Path) -> Result<PathBuf> {
    let root = home.join(".echo-agent").join("sessions");
    for cwd_ent in std::fs::read_dir(&root).with_context(|| format!("read {}", root.display()))? {
        let cwd_ent = cwd_ent?;
        if !cwd_ent.file_type()?.is_dir() {
            continue;
        }
        let candidate = cwd_ent.path().join("prompt_history.jsonl");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("no prompt_history.jsonl found under {}", root.display())
}
