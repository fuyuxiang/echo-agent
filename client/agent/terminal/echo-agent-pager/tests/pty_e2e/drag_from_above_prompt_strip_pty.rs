// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Single-line message; the drag enters it at the tail word.
const STRIPDEEP_LINE: &str = "STRIPDEEP alpha beta gamma delta epsilon";

const ENTRY_WORD: &str = "epsilon";

/// PTY: a mouse-down on the strip directly above the prompt box — OUTSIDE
/// the scrollback pane — arms the anchor-less latch, and dragging up into
/// the last message anchors where the pointer enters text: the payload is
/// the entry-to-release slice. The prompt-gap row is the deterministic
/// representative of the band (turn status and banner need live turn or
/// watcher state the harness can't stage while idle; all band rows share
/// the same arming path).
///
/// `SSH_CONNECTION` forces the OSC 52 clipboard route for readback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn drag_from_above_prompt_strip_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(STRIPDEEP_LINE.to_string());

    let binary = pager_binary().expect("resolve pager binary");
    let overrides: Vec<(String, String)> = vec![(
        "SSH_CONNECTION".into(),
        "scripted-test 1 127.0.0.1 2".into(),
    )];
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut harness = PtyHarness::spawn_with_content_env_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &env_refs,
        Some(content.home()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(ENTRY_WORD, Duration::from_secs(45))
        .expect("message rendered");
    harness
        .wait_for_text("Worked for", Duration::from_secs(20))
        .expect("turn marker rendered");

    harness.inject_keys(b"\t").expect("focus scrollback");
    let _ = harness.wait_for_text("Space:prompt", Duration::from_secs(10));
    harness.update(Duration::from_millis(1500));

    let screen = harness.screen_contents();
    let (entry_row, entry_col) = locate_screen_text(&screen, ENTRY_WORD)
        .unwrap_or_else(|| panic!("could not locate {ENTRY_WORD:?}; screen:\n{screen}"));
    // The strip row sits two rows above the prompt placeholder: placeholder,
    // then the box's top border, then the gap row between the scrollback
    // pane and the prompt box.
    let (placeholder_row, _) = locate_screen_text(&screen, "Build anything")
        .unwrap_or_else(|| panic!("could not locate the prompt placeholder; screen:\n{screen}"));
    let border_row = placeholder_row - 1;
    let border_line = screen.lines().nth(border_row as usize).unwrap_or("");
    assert!(
        border_line.contains('\u{256d}'),
        "setup: prompt top border above the placeholder; line: {border_line:?}"
    );
    let strip_row = border_row - 1;
    let strip_line = screen.lines().nth(strip_row as usize).unwrap_or("");
    assert!(
        strip_line.trim().is_empty(),
        "setup: the press row must be the blank above-prompt strip; line: {strip_line:?}"
    );
    assert!(
        strip_row > entry_row,
        "setup: strip below the message\nscreen:\n{screen}"
    );

    // PRESS on the strip, then drag up into the message. The single motion
    // sample jumps the turn-marker row deliberately (terminals coalesce
    // motion): the same-row column clamp makes the marker's line hittable
    // at any column of its row, and this test pins the strip-to-message
    // path, not marker anchoring. First sample on the message = anchor at
    // the word's first column; then extend to its last column and release.
    let head_col = entry_col + ENTRY_WORD.len() as u16 - 1;
    let mut drag = String::new();
    drag.push_str(&sgr_mouse(0, strip_row, entry_col, 'M'));
    drag.push_str(&sgr_mouse(32, entry_row, entry_col, 'M'));
    drag.push_str(&sgr_mouse(32, entry_row, head_col, 'M'));
    drag.push_str(&sgr_mouse(0, entry_row, head_col, 'm'));
    harness
        .inject_keys(drag.as_bytes())
        .expect("press the above-prompt strip, drag up into the message");

    let payloads = wait_for_osc52_payloads(&mut harness, Duration::from_secs(10));
    assert!(
        !payloads.is_empty(),
        "expected an OSC 52 clipboard write after release; screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    assert_eq!(
        joined, ENTRY_WORD,
        "payload must be the entry-to-release slice of the entered line; \
         payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
