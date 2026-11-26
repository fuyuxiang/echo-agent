//! E2E: stash and restore a draft with the real chord bytes, which the unit tests cannot cover because they inject pre-parsed key events.
//!
//! Legacy terminals send Alt+S as `ESC s`, next to the double-Esc clear: a split decode would arm "press again to clear" and type a literal `s`.
//!
//! ```bash
//! cargo test -p echo-agent-pager-pty-harness --test prompt_stash -- --ignored --nocapture
//! ```

use std::time::Duration;

use anyhow::{Context, Result};
use echo_agent_pager_pty_harness::{ContentController, PtyHarness, pager_binary};

const ROWS: u16 = 50;
const COLS: u16 = 120;
const CANARY: &str = "STASHCANARY99";
const CTRL_CANARY: &str = "CTRLSTASHCANARY7";
/// Top-border caption painted while a draft sits in the stash.
const STASH_CAPTION: &str = "Stashed";
/// Alt+S as legacy terminals encode it: ESC prefix + the letter, one write.
const ALT_S: &[u8] = b"\x1bs";
/// Ctrl+S as every terminal encodes it in raw mode (IXON off): the XOFF byte.
const CTRL_S: &[u8] = b"\x13";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // opt-in: spawns the real pager binary in a PTY (CI runs with --ignored)
async fn chord_bytes_stash_and_restore_the_draft() {
    run().await.expect("stash/pop e2e");
}

async fn run() -> Result<()> {
    let content = ContentController::start()
        .await
        .context("start mock server")?;

    let project = tempfile::tempdir().context("project dir")?;
    std::fs::create_dir_all(project.path().join(".git")).context("create .git")?;

    let binary = pager_binary().context("resolve pager binary")?;
    let mut pager = PtyHarness::spawn_with_content_in_dir(
        &binary,
        ROWS,
        COLS,
        &content,
        &[],
        Some(project.path()),
    )
    .context("spawn pager")?;
    pager
        .wait_for_text("Quit", Duration::from_secs(20))
        .context("welcome screen")?;

    // Draft in the composer, never submitted.
    pager.inject_keys(CANARY.as_bytes()).context("type draft")?;
    pager
        .wait_for_text(CANARY, Duration::from_secs(10))
        .context("draft rendered in composer")?;

    // Stash: the two Alt+S bytes must decode as one Alt+S key.
    pager.inject_keys(ALT_S).context("alt+s stash")?;
    pager
        .wait_for_text(STASH_CAPTION, Duration::from_secs(10))
        .context("caption painted on the prompt border")?;
    pager
        .wait_for_text_absent(CANARY, Duration::from_secs(10))
        .context("composer cleared by stash")?;

    assert!(
        !pager.contains_text("again to clear"),
        "bare-Esc clear arm fired, so ESC+s split-decoded:\n{}",
        pager.screen_contents()
    );

    // Pop: the composer is empty now, so the same chord pops instead of stashing.
    pager.inject_keys(ALT_S).context("alt+s pop")?;
    pager
        .wait_for_text(CANARY, Duration::from_secs(10))
        .context("draft restored by pop")?;
    pager
        .wait_for_text_absent(STASH_CAPTION, Duration::from_secs(10))
        .context("caption clears once the slot is empty")?;

    // Ctrl+S round trip: the stash must outrank the session picker on both legs.
    pager.inject_keys(CTRL_S).context("ctrl+s stash")?;
    pager
        .wait_for_text_absent(CANARY, Duration::from_secs(10))
        .context("first draft re-stashed by ctrl+s")?;

    pager
        .inject_keys(CTRL_CANARY.as_bytes())
        .context("type second draft")?;
    pager
        .wait_for_text(CTRL_CANARY, Duration::from_secs(10))
        .context("second draft rendered")?;

    pager.inject_keys(CTRL_S).context("ctrl+s stash second")?;
    pager
        .wait_for_text_absent(CTRL_CANARY, Duration::from_secs(10))
        .context("second draft stashed by ctrl+s (replacing the first)")?;

    assert!(
        !pager.contains_text("Resume session"),
        "session picker must not open while a draft is stashable:\n{}",
        pager.screen_contents()
    );

    pager.inject_keys(CTRL_S).context("ctrl+s pop second")?;
    pager
        .wait_for_text(CTRL_CANARY, Duration::from_secs(10))
        .context("second draft restored by ctrl+s pop")?;

    assert!(
        !pager.contains_text("panicked"),
        "pager panicked:\n{}",
        pager.screen_contents()
    );

    pager.quit().context("quit pager")?;
    Ok(())
}
