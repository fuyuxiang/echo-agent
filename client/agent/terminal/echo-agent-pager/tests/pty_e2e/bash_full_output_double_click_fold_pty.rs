// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// SGR double-click (press/release × 2) at 0-based (row, col).
fn double_click_at(harness: &mut PtyHarness, row: u16, col: u16) {
    let dbl = format!(
        "{}{}{}{}",
        sgr_mouse(0, row, col, 'M'),
        sgr_mouse(0, row, col, 'm'),
        sgr_mouse(0, row, col, 'M'),
        sgr_mouse(0, row, col, 'm'),
    );
    harness
        .inject_keys(dbl.as_bytes())
        .expect("inject SGR double-click");
}

/// Locate `needle` and double-click its first character cell.
fn double_click_text(harness: &mut PtyHarness, needle: &str) {
    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, needle)
        .unwrap_or_else(|| panic!("locate {needle:?}; screen:\n{screen}"));
    double_click_at(harness, row, col);
}

/// PTY, against the built binary with real SGR clicks: a finished `!`
/// command shows its full output (success and failure), double-click folds
/// the block, and a second double-click restores the full output — never
/// the first/last preview.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
async fn bash_full_output_double_click_fold_pty() {
    let content = ContentController::start().await.expect("start content");
    // Double-click folds only in flash (fold/nav). Pin it so a parallel
    // suite sibling that seeds hold/word_select cannot change semantics.
    seed_ui_config(&content, "keep_text_selection = \"flash\"");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} session ready."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        Some(content.home()),
    )
    .expect("spawn pager");
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");

    // Establish a session so `!` runs as an execute tool with Run chrome.
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("start session");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("session ready");

    // 1. Success: 12 lines exceed the streaming window; all visible on finish.
    //
    // Truncated (default first=2, last=3) shows L01,L02 + L10–L12. A middle
    // line (L06) appears only after expand-on-finish — do not gate on L01:
    // that passes while still truncated and races the L03/L06/L09 asserts.
    // Wait for turn idle first so expand-on-finish has committed before we
    // sample middle lines (suite load used to race wait_for_text("L06")).
    harness
        .inject_keys(b"! printf 'L%02d\\n' $(seq 1 12)\r")
        .expect("submit bash-mode command");
    harness
        .wait_for_text("L12", Duration::from_secs(30))
        .expect("bash output tail");
    harness
        .wait_for_turn_idle(Duration::from_secs(20))
        .expect("bash turn idle before expand-on-finish assert");
    harness
        .wait_for_text("L06", Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "finished ! command must expand full output (middle L06 missing)\nscreen:\n{}",
                harness.screen_contents()
            )
        });
    for line in ["L01", "L03", "L09"] {
        assert!(
            harness.contains_text(line),
            "finished ! command must not truncate output ({line} missing)\nscreen:\n{}",
            harness.screen_contents()
        );
    }

    // 2. Double-click folds; a second double-click restores the full output.
    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Ctrl+e:", Duration::from_secs(10))
        .expect("scrollback owns keys");
    // Retry the fold gesture: a single SGR burst can miss under load if the
    // header cell moved between locate and inject, or if multi-click state
    // from an earlier accidental click is still open.
    let fold_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        double_click_text(&mut harness, "Run (user)");
        if harness
            .wait_for_text_absent("L06", Duration::from_secs(3))
            .is_ok()
        {
            break;
        }
        if Instant::now() >= fold_deadline {
            panic!(
                "double-click must collapse the ! block; got:\n{}",
                harness.screen_contents()
            );
        }
        // MULTI_CLICK_TIMEOUT_MS is 300ms; clear before retrying.
        harness.update(Duration::from_millis(500));
    }
    // MULTI_CLICK_TIMEOUT_MS is 300ms; clear it before the expand gesture so
    // the second double-click is not counted as click 3/4 of the first.
    harness.update(Duration::from_millis(500));
    // Re-locate: collapse shrinks the block and may move the header on screen.
    double_click_text(&mut harness, "Run (user)");
    harness
        .wait_for_text("L06", Duration::from_secs(15))
        .unwrap_or_else(|_| {
            panic!(
                "double-click must restore the FULL output (middle lines); got:\n{}",
                harness.screen_contents()
            )
        });

    // 3. A failing command finishes fully expanded too.
    harness.inject_keys(b"\t").expect("refocus prompt");
    harness
        .wait_for_text("Shift+Tab:mode", Duration::from_secs(10))
        .expect("prompt owns keys");
    harness
        .inject_keys(b"! printf 'E%02d\\n' $(seq 1 12); false\r")
        .expect("submit failing bash-mode command");
    harness
        .wait_for_text("E12", Duration::from_secs(30))
        .expect("failed bash output tail");
    harness
        .wait_for_turn_idle(Duration::from_secs(20))
        .expect("failed bash turn idle before expand-on-finish assert");
    harness
        .wait_for_text("E06", Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "FAILED ! command must show its full output; got:\n{}",
                harness.screen_contents()
            )
        });

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
