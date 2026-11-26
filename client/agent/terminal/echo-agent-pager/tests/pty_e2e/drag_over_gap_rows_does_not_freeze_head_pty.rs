// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Paragraph tokens above and below the blank row inside one message.
const GAP_TOP: &str = "GAPROW_ALPHA";

const GAP_BOTTOM: &str = "GAPROW_OMEGA";

/// PTY: a drag whose LAST motion lands on the dead blank row between two
/// paragraphs of one message must not freeze the head — the copy spans the
/// gap into the second paragraph.
///
/// Both paragraphs share the message body's selection range; the blank row
/// between them has no selectable columns, so before dead-zone snapping the
/// head collapsed to the anchor there (a one-character copy).
/// `SSH_CONNECTION` forces the OSC 52 clipboard route for readback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn drag_over_gap_rows_does_not_freeze_head_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!(
        "{GAP_TOP} first paragraph line\n\n{GAP_BOTTOM} second paragraph line"
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
        .wait_for_text(GAP_BOTTOM, Duration::from_secs(45))
        .expect("both paragraphs rendered");

    harness.inject_keys(b"\t").expect("focus scrollback");
    let _ = harness.wait_for_text("Space:prompt", Duration::from_secs(10));
    // Settle: the turn-end relayout can shift rows; locate afterwards.
    harness.update(Duration::from_millis(1500));

    let screen = harness.screen_contents();
    let (row_top, col_top) = locate_screen_text(&screen, GAP_TOP)
        .unwrap_or_else(|| panic!("could not locate {GAP_TOP:?}; screen:\n{screen}"));
    let (row_bottom, _) = locate_screen_text(&screen, GAP_BOTTOM)
        .unwrap_or_else(|| panic!("could not locate {GAP_BOTTOM:?}; screen:\n{screen}"));
    assert_eq!(
        row_bottom,
        row_top + 2,
        "setup: exactly one blank row between the paragraphs\nscreen:\n{screen}"
    );
    let gap_row = row_top + 1;
    let drag_col = col_top + 12;
    let gap_line = screen.lines().nth(gap_row as usize).unwrap_or("");
    assert!(
        gap_line
            .chars()
            .skip(col_top as usize)
            .take(13)
            .all(char::is_whitespace),
        "setup: the gap row must be blank at the drag columns; line: {gap_line:?}"
    );

    // Press on the first paragraph, drag ONCE onto the dead row, release there.
    let mut drag = String::new();
    drag.push_str(&sgr_mouse(0, row_top, col_top, 'M'));
    drag.push_str(&sgr_mouse(32, gap_row, drag_col, 'M'));
    drag.push_str(&sgr_mouse(0, gap_row, drag_col, 'm'));
    harness
        .inject_keys(drag.as_bytes())
        .expect("drag onto the gap row and release");

    let payloads = wait_for_osc52_payloads(&mut harness, Duration::from_secs(10));
    assert!(
        !payloads.is_empty(),
        "expected an OSC 52 clipboard write after release; screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    assert!(
        joined.contains(GAP_TOP),
        "clipboard must contain the anchor paragraph; payloads={payloads:?}"
    );
    assert!(
        joined.contains(GAP_BOTTOM),
        "clipboard span must cross the dead row into the second paragraph; \
         payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
