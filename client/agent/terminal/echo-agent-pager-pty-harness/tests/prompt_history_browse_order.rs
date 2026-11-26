//! E2E: the Up browse is one reverse-chronological list holding every kind of thing typed at the
//! composer, interleaved in send order.
//!
//! The panel paints oldest at the top, so a newest-first list lands on strictly descending rows.
//! `#` notes need `[features] remember_mode = true`, which the run seeds into the sandbox
//! `$ECHO_AGENT_HOME/config.toml`.

use std::time::Duration;

use anyhow::{Context, Result};
use echo_agent_pager_pty_harness::{ContentController, PtyHarness, keys, pager_binary};

const ROWS: u16 = 50;
const COLS: u16 = 120;
/// One sentinel per turn, so waiting for the second turn cannot match the first turn's block.
const FIRST_ACK: &str = "ACKSENTINELONE";
const SECOND_ACK: &str = "ACKSENTINELTWO";

const FIRST_PROMPT: &str = "hello";
const FIRST_COMMAND: &str = "! echo one";
const SLASH_COMMAND: &str = "/session-info";
const NOTE_TEXT: &str = "deploys need the staging flag";
const SECOND_PROMPT: &str = "second prompt";
const SECOND_COMMAND: &str = "! echo two";

/// What the browse must hold, newest first. The `! ` prefix is part of the entry: recall reads it
/// back into shell mode.
const EXPECTED_NEWEST_FIRST: [&str; 6] = [
    SECOND_COMMAND,
    SECOND_PROMPT,
    NOTE_TEXT,
    SLASH_COMMAND,
    FIRST_COMMAND,
    FIRST_PROMPT,
];

/// Bash-mode indicator on the prompt info row.
const SHELL_MODE_LABEL: &str = "Run shell command";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // opt-in: spawns the real pager binary in a PTY (CI runs with --ignored)
async fn up_arrow_browse_interleaves_prompts_commands_and_notes() {
    run().await.expect("prompt-history browse-order e2e");
}

async fn run() -> Result<()> {
    let content = ContentController::start()
        .await
        .context("start mock server")?;
    content.set_response(format!("{FIRST_ACK} acknowledged."));

    // `#` notes are gated off by default, so the sixth entry needs the flag before the pager boots.
    let echo_agent_home = content.home().join(".echo-agent");
    std::fs::create_dir_all(&echo_agent_home).context("create sandbox .echo-agent")?;
    std::fs::write(
        echo_agent_home.join("config.toml"),
        "[features]\nremember_mode = true\n",
    )
    .context("seed remember_mode config")?;

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

    // 1. Plain prompt. Also opens the session the rest of the sends need.
    pager
        .inject_keys(format!("{FIRST_PROMPT}\r").as_bytes())
        .context("submit first prompt")?;
    pager
        .wait_for_text(FIRST_ACK, Duration::from_secs(30))
        .context("first turn rendered")?;
    pager
        .wait_for_turn_idle(Duration::from_secs(20))
        .context("first turn idle")?;

    // 2. Shell command. `!` on the empty composer enters bash mode.
    send_shell_command(&mut pager, FIRST_COMMAND)?;

    // 3. Slash command. Resolves in the client and never reaches the shell.
    pager
        .inject_keys(format!("{SLASH_COMMAND}\r").as_bytes())
        .context("submit slash command")?;
    pager
        .wait_for_text("Session info", Duration::from_secs(20))
        .context("session-info modal")?;
    pager.inject_keys(keys::ESC).context("close modal")?;
    pager
        .wait_for_text_absent("Session info", Duration::from_secs(10))
        .context("session-info modal closed")?;

    // 4. Memory note. `#` on the empty composer enters remember mode.
    pager
        .inject_keys(format!("#{NOTE_TEXT}\r").as_bytes())
        .context("submit memory note")?;
    pager
        .wait_for_text("Memory Note", Duration::from_secs(20))
        .context("memory-note review modal")?;
    pager.inject_keys(keys::ESC).context("close note modal")?;
    pager
        .wait_for_text_absent("Memory Note", Duration::from_secs(10))
        .context("memory-note modal closed")?;

    // 5. Second plain prompt.
    content.set_response(format!("{SECOND_ACK} acknowledged."));
    pager
        .inject_keys(format!("{SECOND_PROMPT}\r").as_bytes())
        .context("submit second prompt")?;
    pager
        .wait_for_text(SECOND_ACK, Duration::from_secs(30))
        .context("second turn rendered")?;
    pager
        .wait_for_turn_idle(Duration::from_secs(20))
        .context("second turn idle")?;

    // 6. Second shell command.
    send_shell_command(&mut pager, SECOND_COMMAND)?;

    // Up on the empty composer opens the browse.
    pager.inject_keys(keys::UP).context("open browse")?;
    pager
        .wait_until(
            "history panel to list every send",
            Duration::from_secs(15),
            |h| panel_rows(&h.screen_contents()).len() >= EXPECTED_NEWEST_FIRST.len(),
        )
        .context("history panel populated")?;

    let screen = pager.screen_contents();
    let rows = panel_rows(&screen);

    let mut positions = Vec::new();
    for entry in EXPECTED_NEWEST_FIRST {
        let hits: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.contains(entry))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "{entry:?} must appear exactly once in the browse, found {hits:?}\npanel rows: \
             {rows:#?}\nscreen:\n{screen}"
        );
        positions.push(hits[0]);
    }

    // Oldest at the top, so the newest-first list must read as strictly descending row indices.
    for pair in positions.windows(2) {
        assert!(
            pair[0] > pair[1],
            "browse is not one reverse-chronological list: expected newest-first \
             {EXPECTED_NEWEST_FIRST:?}, got row indices {positions:?}\npanel rows: {rows:#?}"
        );
    }
    assert_eq!(
        rows.len(),
        EXPECTED_NEWEST_FIRST.len(),
        "browse holds entries nobody typed\npanel rows: {rows:#?}\nscreen:\n{screen}"
    );

    // The newest entry is selected and populated, so the composer is in shell mode.
    assert!(
        pager.contains_text(SHELL_MODE_LABEL),
        "recalling {SECOND_COMMAND:?} must put the composer in shell mode:\n{}",
        pager.screen_contents()
    );

    // One step up is the plain prompt, which must drop shell mode again.
    pager.inject_keys(keys::UP).context("step to the prompt")?;
    pager
        .wait_for_text_absent(SHELL_MODE_LABEL, Duration::from_secs(10))
        .context("recalling a plain prompt leaves shell mode")?;

    assert!(
        !pager.contains_text("panicked"),
        "pager panicked:\n{}",
        pager.screen_contents()
    );

    pager.quit().context("quit pager")?;
    Ok(())
}

/// Type `command` (a `! `-prefixed shell command) and submit it.
///
/// The bash-mode label is the synchronization point at both ends: it proves the leading `!` armed
/// shell mode, and its disappearance proves the send consumed the composer.
fn send_shell_command(pager: &mut PtyHarness, command: &str) -> Result<()> {
    pager
        .inject_keys(command.as_bytes())
        .with_context(|| format!("type {command:?}"))?;
    pager
        .wait_for_text(SHELL_MODE_LABEL, Duration::from_secs(10))
        .with_context(|| format!("{command:?} armed shell mode"))?;
    pager.inject_keys(keys::ENTER).context("submit")?;
    pager
        .wait_for_text_absent(SHELL_MODE_LABEL, Duration::from_secs(20))
        .with_context(|| format!("{command:?} left the composer"))?;
    pager
        .wait_for_turn_idle(Duration::from_secs(20))
        .with_context(|| format!("{command:?} finished"))
}

/// Rows of the open history panel, top (oldest) to bottom (newest, against the composer).
///
/// The panel is bounded by two horizontal rules; the top one carries the ` history ` caption.
fn panel_rows(screen: &str) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let Some(top) = lines
        .iter()
        .position(|line| line.contains("\u{2500} history "))
    else {
        return Vec::new();
    };
    lines[top + 1..]
        .iter()
        .take_while(|line| !is_rule(line))
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

/// Whether the line is one of the panel's horizontal rules.
fn is_rule(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && trimmed.chars().all(|c| c == '\u{2500}')
}
