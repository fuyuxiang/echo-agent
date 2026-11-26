// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Content of the nested file — `cat`ing it through the completed, quoted
/// command must print this into the scrollback (proves the inserted quoting
/// parses as a valid command in the real shell).
const INNER_SENTINEL: &str = "INNER-NOTE-SENTINEL-4173";

/// Env: NO suggestion flag — Tab completion in bash mode is always on, and
/// this test is the acceptance proof. `ECHO_AGENT_SUGGESTIONS=0` pins the
/// as-you-type pipeline OFF hermetically (the PTY child inherits the parent
/// env, so a dev shell exporting the flag must not turn it on here); the
/// history tier is pinned to a nonexistent file so file completions are the
/// ONLY dropdown source.
fn suggestions_env(content: &ContentController) -> Vec<(String, String)> {
    vec![
        ("SHELL".into(), "/bin/bash".into()),
        ("ECHO_AGENT_SUGGESTIONS".into(), "0".into()),
        (
            "HISTFILE".into(),
            content
                .home()
                .join(".no_such_history")
                .to_string_lossy()
                .into_owned(),
        ),
    ]
}

/// Seed the session cwd the file provider lists:
/// - `alpha_one.txt` + `alpha_two.txt` — the common-prefix-fill pair;
/// - `notes.md` + `Notes Archive/inner_note.txt` — exact vs case-insensitive
///   candidates, a spaced directory to drill into, and the file to run;
/// - `script.sh` — unrelated noise that must never match either prefix.
fn seed_cwd(cwd: &Path) {
    std::fs::create_dir_all(cwd.join(".git")).expect("create .git");
    std::fs::write(cwd.join("alpha_one.txt"), "").expect("seed alpha_one");
    std::fs::write(cwd.join("alpha_two.txt"), "").expect("seed alpha_two");
    std::fs::write(cwd.join("notes.md"), "").expect("seed notes.md");
    std::fs::write(cwd.join("script.sh"), "").expect("seed script.sh");
    let dir = cwd.join("Notes Archive");
    std::fs::create_dir_all(&dir).expect("seed spaced dir");
    std::fs::write(dir.join("inner_note.txt"), format!("{INNER_SENTINEL}\n"))
        .expect("seed inner note");
}

/// The Tab-armed fetch and the post-accept/fill refreshes are async; give a
/// landed response comfortable slack before the next Tab consumes it.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1500)).await;
}

/// **Bash-mode file completion behaves like a real shell — with NO env
/// flag.** In a seeded sandbox cwd:
/// - `!cat al` + Tab fills the shared prefix `alpha_` in place (no dropdown
///   flash); the next Tab opens the dropdown listing both candidates;
/// - `cat "no` + Tab opens the dropdown (exact `notes.md` above the
///   case-insensitive `Notes Archive/`); Down+Tab accepts the directory,
///   preserving the open quote and keeping it open for drill-down;
/// - the next Tab finds exactly one candidate inside the directory and
///   accepts it immediately, closing the quote;
/// - Enter runs the completed command through the real shell — the file's
///   sentinel content reaching the scrollback proves the quoting produced a
///   valid command.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
#[cfg(unix)]
async fn bash_mode_file_completion_shell_like() {
    let project = tempfile::tempdir().expect("create project dir");
    seed_cwd(project.path());
    let cwd = dunce::canonicalize(project.path()).expect("canonicalize project");

    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} session up."));

    let env = suggestions_env(&content);
    let env_refs: Vec<(&str, &str)> = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &env_refs,
        Some(&cwd),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Establish a session: bash mode lives on the agent-view prompt (the
    // welcome prompt never completes).
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("start session");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("session ready");

    // ── Leg 1: Tab fills the common prefix, second Tab opens the list ───
    harness
        .inject_keys(b"!cat al")
        .expect("type bash prefix with shared-prefix candidates");
    harness.inject_keys(b"\t").expect("first Tab");
    harness
        .wait_for_text("cat alpha_", Duration::from_secs(10))
        .expect("first Tab filled the common prefix in place");
    let screen = harness.screen_contents();
    assert!(
        !screen.contains("alpha_one.txt"),
        "prefix fill must not flash the dropdown:\n{screen}"
    );

    settle().await;
    harness.inject_keys(b"\t").expect("second Tab");
    harness
        .wait_for_text("alpha_one.txt", Duration::from_secs(10))
        .expect("second Tab opened the dropdown");
    harness
        .wait_for_text("alpha_two.txt", Duration::from_secs(5))
        .expect("both candidates listed");

    // Close the dropdown and clear the draft (non-empty: bash mode sticks).
    harness.inject_keys(b"\x1b").expect("Esc closes dropdown");
    harness.inject_keys(b"\x15").expect("Ctrl+U clears draft");

    // ── Leg 2: quoted dropdown → dir accept → drill-down insta-accept ───
    harness
        .inject_keys(b"cat \"no")
        .expect("type quoted prefix");
    harness.inject_keys(b"\t").expect("Tab opens dropdown");
    // Case-differing candidates share no common prefix: no fill, plain open.
    harness
        .wait_for_text("Notes Archive/", Duration::from_secs(10))
        .expect("dropdown lists the case-insensitive directory match");

    harness
        .inject_keys(b"\x1b[B")
        .expect("Down to the directory");
    harness
        .inject_keys(b"\t")
        .expect("Tab accepts the directory");
    harness
        .wait_for_text("cat \"Notes Archive/", Duration::from_secs(10))
        .expect("directory accepted with the quote preserved and open");

    // The accept re-fetched inside the directory; its single candidate
    // insta-accepts and closes the quote.
    settle().await;
    harness.inject_keys(b"\t").expect("Tab drills down");
    harness
        .wait_for_text("Notes Archive/inner_note.txt\"", Duration::from_secs(10))
        .expect("single inner candidate accepted immediately, quote closed");

    // ── The completed command actually RUNS through the real shell ──────
    harness.inject_keys(b"\r").expect("Enter runs the command");
    harness
        .wait_for_text(INNER_SENTINEL, Duration::from_secs(30))
        .expect("cat printed the nested file through the completed quoting");

    let screen = harness.screen_contents();
    assert!(
        !screen.contains("panicked"),
        "pager panicked\nscreen:\n{screen}"
    );

    harness.quit().expect("clean quit");
}
