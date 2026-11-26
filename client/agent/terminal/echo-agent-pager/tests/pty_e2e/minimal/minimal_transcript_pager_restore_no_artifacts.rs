// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Frame-write delay (ms) widening the suspend-vs-queued-frame race window:
/// the writer thread holds every frame this long, so any frame still queued
/// when the suspend runs is guaranteed to land during the `$PAGER` child's
/// tenure (on its alternate screen) unless the pre-suspend drain waits for it.
const FRAME_DELAY_MS: &str = "40";

/// Dogfood bug: after returning from the minimal `/transcript` pager the live
/// region was "off by one" — a stale status row above a freshly drawn prompt,
/// stray `[` escape fragments, the cursor on the info row. Root cause: frames
/// are written to the tty by an async writer thread, and the suspend path
/// never drained it — the frame that armed the suspend (plus any composer
/// clear / viewport scroll frames queued just before) raced `less`, landing on
/// its alternate screen. The main screen never received those writes, so after
/// `rmcup` restored it, the renderer's diff no longer described reality and
/// the stale rows were never repainted.
///
/// The fix drains the writer before the child takes the tty (deterministic
/// coverage: the `WriterSync` unit tests in `render::draw`), then re-anchors
/// and full-repaints after it exits. This test drives the user-visible round
/// trip — burst of composer edits, `/transcript`, a real `less` (alt screen +
/// `rmcup`), `q` — under the frame-write delay, and asserts the restored
/// screen is exactly the idle live region: one status row, one info row, no
/// stale draft/command text, no torn escape fragments.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_transcript_pager_restore_no_artifacts() {
    // A real interactive pager is the point (alt screen + rmcup restore).
    if std::process::Command::new("less")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping: `less` not available on this machine");
        return;
    }

    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} transcript body."));

    let mut overrides: Vec<(String, String)> = vec![("PAGER".to_string(), "less".to_string())];
    overrides.push((
        "ECHO_AGENT_TEST_FRAME_WRITE_DELAY_MS".to_string(),
        FRAME_DELAY_MS.to_string(),
    ));
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        MINIMAL_ARGS,
        &env_refs,
    )
    .expect("spawn minimal pager");
    harness.set_respond_to_queries(true);

    wait_minimal_ready(&mut harness);

    // A committed turn so the transcript has content.
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_full_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn committed");
    // Let the (delayed) writer queue fully drain at idle so the burst below is
    // the only thing in flight when the suspend arms.
    harness.update(Duration::from_secs(3));

    // Queue a burst of composer-changing frames right before the suspend:
    // type a draft, kill it (Ctrl+U), then type /transcript (slash dropdown
    // opens + closes: viewport grow/shrink frames) and submit. With the
    // writer delay, the trailing clear/shrink frames are still queued when
    // the suspend path runs — the shape of the dogfood report.
    inject_keys_paced(&mut harness, b"zetaquxdraft");
    harness.update(Duration::from_millis(120));
    harness.inject_keys(b"\x15").expect("Ctrl+U kill draft");
    inject_keys_paced(&mut harness, b"/transcript");
    harness.inject_keys(b"\r").expect("submit /transcript");

    // less (alt screen, `+G`) is foreground once its status line shows the
    // transcript temp-file path — the conversation sentinel can't be used as
    // the signal, it is already visible on the live screen. Then quit it.
    harness
        .wait_for_text("echo-agent-transcript-", Duration::from_secs(20))
        .expect("less shows the transcript");
    harness.update(Duration::from_millis(300));
    harness.inject_keys(b"q").expect("quit less");

    // Restored: the idle live region, with no artifacts. Give the resume
    // redraw (and any stragglers) time to settle before asserting.
    harness
        .wait_for_text(MINIMAL_IDLE_SENTINEL, Duration::from_secs(10))
        .expect("inline TUI restored after less exited");
    harness.update(Duration::from_secs(2));

    let screen = harness.screen_contents();
    // Exactly one status row and one info row on the visible screen: a lost
    // scroll leaves a stale extra copy of one of them (the "off by one" look).
    assert_eq!(
        screen.matches(MINIMAL_IDLE_SENTINEL).count(),
        1,
        "exactly one idle status row after the pager round trip\nscreen:\n{screen}"
    );
    assert_eq!(
        screen.matches("ctrl+o transcript").count(),
        1,
        "exactly one info row after the pager round trip\nscreen:\n{screen}"
    );
    // The frames queued right before the suspend (composer kill + /transcript
    // submit-clear) must have LANDED: without the pre-suspend writer drain
    // they die on the pager's alternate screen, and the restored main screen
    // still shows the killed draft / the submitted command in the prompt while
    // the renderer's diff believes both are gone — exactly the stale-row
    // corruption from the dogfood report.
    assert!(
        !screen.contains("❯ /transcript"),
        "stale submitted command left in the prompt after the round trip\nscreen:\n{screen}"
    );
    assert!(
        !screen.contains("zetaquxdraft"),
        "stale killed-draft text left on screen after the round trip\nscreen:\n{screen}"
    );
    // No torn-escape fragments at the left margin (the dogfood `[Claud…` row):
    // no screen row may START with a literal `[`.
    assert!(
        !screen.lines().any(|l| l.starts_with('[')),
        "no torn escape fragments at column 0\nscreen:\n{screen}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{screen}"
    );

    quit_minimal(&mut harness);
}
