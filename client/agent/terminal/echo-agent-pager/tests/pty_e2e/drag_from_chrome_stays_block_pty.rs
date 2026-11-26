// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Paragraph tokens above and below the blank row inside one message.
const CHROMEHOLD_TOP: &str = "CHROMEHOLD_TOP";

const CHROMEHOLD_BOTTOM: &str = "CHROMEHOLD_BOTTOM";

/// PTY: a mouse-down on a blank row INSIDE a block's area whose drag never
/// touches selectable text stays a whole-block drag: release copies the
/// whole message (both paragraphs), exactly as before the deferred-anchor
/// latch existed. Covers the never-enter branch the anchor-on-entry
/// conversion must not disturb.
///
/// `SSH_CONNECTION` forces the OSC 52 clipboard route for readback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn drag_from_chrome_stays_block_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!(
        "{CHROMEHOLD_TOP} first paragraph line\n\n{CHROMEHOLD_BOTTOM} second paragraph line"
    ));

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
        .wait_for_text(CHROMEHOLD_BOTTOM, Duration::from_secs(45))
        .expect("both paragraphs rendered");

    harness.inject_keys(b"\t").expect("focus scrollback");
    let _ = harness.wait_for_text("Space:prompt", Duration::from_secs(10));
    harness.update(Duration::from_millis(1500));

    let screen = harness.screen_contents();
    let (row_top, col_top) = locate_screen_text(&screen, CHROMEHOLD_TOP)
        .unwrap_or_else(|| panic!("could not locate {CHROMEHOLD_TOP:?}; screen:\n{screen}"));
    let (row_bottom, _) = locate_screen_text(&screen, CHROMEHOLD_BOTTOM)
        .unwrap_or_else(|| panic!("could not locate {CHROMEHOLD_BOTTOM:?}; screen:\n{screen}"));
    assert_eq!(
        row_bottom,
        row_top + 2,
        "setup: exactly one blank row between the paragraphs\nscreen:\n{screen}"
    );
    let chrome_row = row_top + 1;
    let chrome_line = screen.lines().nth(chrome_row as usize).unwrap_or("");
    assert!(
        chrome_line
            .chars()
            .skip(col_top as usize)
            .take(16)
            .all(char::is_whitespace),
        "setup: the in-block row must be blank at the drag columns; line: {chrome_line:?}"
    );

    // PRESS on the blank in-block row and drag sideways along it — the
    // pointer never enters selectable text, so the gesture stays a block
    // drag — then release.
    let mut drag = String::new();
    drag.push_str(&sgr_mouse(0, chrome_row, col_top, 'M'));
    drag.push_str(&sgr_mouse(32, chrome_row, col_top + 3, 'M'));
    drag.push_str(&sgr_mouse(0, chrome_row, col_top + 3, 'm'));
    harness
        .inject_keys(drag.as_bytes())
        .expect("press the in-block blank row, drag along it");

    let payloads = wait_for_osc52_payloads(&mut harness, Duration::from_secs(10));
    assert!(
        !payloads.is_empty(),
        "expected an OSC 52 clipboard write after release; screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    assert!(
        joined.starts_with(&format!("{CHROMEHOLD_TOP} first paragraph line")),
        "whole-block copy starts at the block's first line; payloads={payloads:?}"
    );
    assert!(
        joined
            .trim_end()
            .ends_with(&format!("{CHROMEHOLD_BOTTOM} second paragraph line")),
        "whole-block copy includes the full second paragraph; payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
