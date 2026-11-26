// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Title used for the manual `/rename`; unique so screen scans are unambiguous.
const RENAME_TITLE: &str = "PTYRENAMETITLE";

/// The prompt-box top-border row carrying the inline title: the line holding
/// both the title and the `╮` corner. Line-oriented on purpose — never
/// byte-slice box-drawing rows (char-boundary panics).
fn title_border_row(screen: &str, title: &str) -> Option<String> {
    screen
        .lines()
        .find(|line| line.contains(title) && line.contains('\u{256e}'))
        .map(str::to_string)
}

/// Poll until the border title renders (the rename lands asynchronously),
/// then return its row.
fn wait_for_title_row(harness: &mut PtyHarness, title: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        harness.update(Duration::from_millis(200));
        let screen = harness.screen_contents();
        if let Some(row) = title_border_row(&screen, title) {
            return row;
        }
        if Instant::now() >= deadline {
            panic!("no top-border row with the title\nscreen:\n{screen}");
        }
    }
}

/// The top-border row of the live prompt box: the `╮` row whose successor is
/// the prompt's text row (`│ ❯ …` — side border and/or prefix). Anchoring to
/// the prompt keeps the negative (no-title) assert honest even if some other
/// widget ever draws a plain top border.
fn prompt_top_border_row(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.lines().collect();
    lines.windows(2).find_map(|pair| {
        (pair[0].contains('\u{256e}')
            && (pair[1].trim_start().starts_with('\u{2502}') || pair[1].contains('\u{276f}')))
        .then(|| pair[0].to_string())
    })
}

/// Whether `row` is a fully plain `╭──…──╮` border (i.e. no inline title).
fn is_plain_border_row(row: &str) -> bool {
    let trimmed = row.trim();
    let mut chars = trimmed.chars();
    trimmed.chars().count() > 2
        && chars.next() == Some('\u{256d}')
        && chars.next_back() == Some('\u{256e}')
        && chars.all(|c| c == '\u{2500}')
}

/// Assert the title run on the border row carries the info-line text
/// treatment: not inverse, not bold, same background as the plain border
/// cells (no chip), and a foreground differing from the border rule (deltas,
/// not exact theme hex).
fn assert_title_styled(harness: &mut PtyHarness, title: &str) {
    let styled = harness.screen_styled();
    let title_line = styled
        .iter()
        .find(|line| {
            let text: String = line.runs.iter().map(|run| run.text.as_str()).collect();
            text.contains(title) && text.contains('\u{256e}')
        })
        .unwrap_or_else(|| {
            panic!(
                "no styled top-border row with the title\nscreen:\n{}",
                harness.screen_contents()
            )
        });
    let title_run = title_line
        .runs
        .iter()
        .find(|run| run.text.contains(title))
        .expect("title run on the border row");
    let border_run = title_line
        .runs
        .iter()
        .find(|run| run.text.contains('\u{2500}') && !run.text.contains(title))
        .expect("plain border run on the title row");
    assert!(
        !title_run.inverse,
        "border title must not be inverse: {title_run:?}"
    );
    assert!(
        !title_run.bold,
        "border title must not be bold: {title_run:?}"
    );
    assert_eq!(
        title_run.bg, border_run.bg,
        "border title keeps the plain border background (no chip)"
    );
    assert_ne!(
        title_run.fg, border_run.fg,
        "border title foreground must differ from the border rule"
    );
}

/// Graceful quit via the full-TUI chord: double Ctrl+Q, 200ms apart (the
/// prompt owns plain keys and Ctrl+C is a no-op outside minimal mode — see
/// `continue_resumes_session_with_history`), then reap and assert exit 0 so
/// the shell finishes teardown before a respawn reuses the same HOME.
fn quit_gracefully(mut harness: PtyHarness) {
    harness.update(Duration::from_millis(300));
    harness.inject_keys(b"\x11").expect("ctrl-q arm");
    harness.update(Duration::from_millis(200));
    harness.inject_keys(b"\x11").expect("ctrl-q confirm");
    let exit = wait_for_exit_status(&mut harness, Duration::from_secs(10))
        .expect("wait for graceful quit");
    assert_eq!(
        exit,
        PtyExitPoll::Exited(0),
        "graceful quit should exit 0, got {exit:?}"
    );
}

/// Spawn a pager in `project` against `content`, submit one turn, and settle.
fn spawn_settled_session(content: &ContentController, project: &Path) -> PtyHarness {
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        content,
        &[],
        Some(project),
    )
    .expect("spawn pager");
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered");
    harness.update(Duration::from_millis(1000)); // let the short turn settle
    harness
}

/// Type `/rename <title>` (paced — bulk injects paste-coalesce post-turn),
/// submit it, and wait for the durable-write ack: the border title itself
/// renders from the optimistic local `display_name`, but the "Session renamed
/// to" block only lands after the shell's locked `summary.json` write
/// round-trips.
fn submit_rename(harness: &mut PtyHarness, title: &str) {
    inject_keys_paced(harness, format!("/rename {title}").as_bytes());
    harness.inject_keys(b"\r").expect("submit /rename");
    harness
        .wait_for_text("Session renamed to", Duration::from_secs(15))
        .expect("rename durable-write ack");
}

/// A manual `/rename` renders the session title inline on the prompt box's
/// top border (info-line dim text treatment), right-aligned before the `╮`
/// corner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn rename_title_shows_in_prompt_border() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} rename title turn."));

    let project = tempfile::tempdir().expect("project dir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");

    let mut harness = spawn_settled_session(&content, project.path());
    submit_rename(&mut harness, RENAME_TITLE);

    let title_row = wait_for_title_row(&mut harness, RENAME_TITLE, Duration::from_secs(10));
    assert!(
        title_row.contains('\u{2500}'),
        "title row must still be a border row: {title_row}"
    );
    assert_title_styled(&mut harness, RENAME_TITLE);

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    quit_gracefully(harness);
}

/// The border title survives quit → `--continue`: manual-ness is persisted in
/// `summary.json` (`title_is_manual`) and re-hydrated into `display_name` on
/// resume. The inverse holds too: a session that was never renamed resumes
/// with a plain top border (auto titles never show on the border).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn rename_title_survives_resume_and_stays_absent_without_rename() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} resume title turn."));

    // Sessions are keyed by cwd: one project per sub-case, same isolated HOME.
    let renamed_project = tempfile::tempdir().expect("renamed project dir");
    std::fs::create_dir_all(renamed_project.path().join(".git")).expect("create .git");
    let plain_project = tempfile::tempdir().expect("plain project dir");
    std::fs::create_dir_all(plain_project.path().join(".git")).expect("create .git");

    // Renamed session: border title shows, then quit.
    let mut first = spawn_settled_session(&content, renamed_project.path());
    submit_rename(&mut first, RENAME_TITLE);
    wait_for_title_row(&mut first, RENAME_TITLE, Duration::from_secs(15));
    quit_gracefully(first);

    // Never-renamed session: plain border, then quit.
    let plain_first = spawn_settled_session(&content, plain_project.path());
    quit_gracefully(plain_first);

    let binary = pager_binary().expect("resolve pager binary");

    // Resume the renamed session: the border title returns without re-renaming.
    let mut resumed = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--continue"],
        Some(renamed_project.path()),
    )
    .expect("spawn resumed pager");
    resumed
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("resumed session replayed history");
    wait_for_title_row(&mut resumed, RENAME_TITLE, Duration::from_secs(15));
    assert_title_styled(&mut resumed, RENAME_TITLE);
    assert!(
        !resumed.contains_text("panicked"),
        "pager panicked on resume\nscreen:\n{}",
        resumed.screen_contents()
    );
    quit_gracefully(resumed);

    // Resume the never-renamed session: top border stays plain.
    let mut plain_resumed = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--continue"],
        Some(plain_project.path()),
    )
    .expect("spawn plain resumed pager");
    plain_resumed
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("plain resumed session replayed history");
    // Let the async title hydration land before asserting the border stayed plain.
    plain_resumed.update(Duration::from_secs(2));
    let screen = plain_resumed.screen_contents();
    let border = prompt_top_border_row(&screen)
        .unwrap_or_else(|| panic!("no prompt top-border row found\nscreen:\n{screen}"));
    assert!(
        is_plain_border_row(&border),
        "prompt top border must stay plain without a rename, got: {border}\nscreen:\n{screen}"
    );
    assert!(
        !plain_resumed.contains_text("panicked"),
        "pager panicked on plain resume\nscreen:\n{screen}"
    );
    quit_gracefully(plain_resumed);
}

/// `/rename --auto` clears the prompt-border title. Followers / picker
/// converge later when a regenerated auto title is adopted; this test
/// pins the initiating pager's optimistic clear.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn rename_auto_clears_prompt_border_title() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} unpin title turn."));

    let project = tempfile::tempdir().expect("project dir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");

    let mut harness = spawn_settled_session(&content, project.path());
    submit_rename(&mut harness, RENAME_TITLE);
    wait_for_title_row(&mut harness, RENAME_TITLE, Duration::from_secs(10));

    inject_keys_paced(&mut harness, b"/rename --auto");
    harness.inject_keys(b"\r").expect("submit /rename --auto");
    harness
        .wait_for_text("Session title reset to auto", Duration::from_secs(15))
        .expect("unpin durable-write ack");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.update(Duration::from_millis(200));
        let screen = harness.screen_contents();
        if let Some(border) = prompt_top_border_row(&screen)
            && is_plain_border_row(&border)
        {
            break;
        }
        if Instant::now() >= deadline {
            panic!("prompt top border must be plain after --auto\nscreen:\n{screen}");
        }
    }

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    quit_gracefully(harness);
}
