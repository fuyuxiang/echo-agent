// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
use echo_agent_pager_pty_harness::StyledLine;

/// Per-cell `(bg, inverse)` styling for one screen line (`StyledLine.line` is
/// 1-based), expanded from its runs in column order. Selection highlight shows
/// up as a changed `bg` and/or `inverse` on these cells.
fn row_cells(lines: &[StyledLine], target_line: usize) -> Vec<(Option<String>, bool)> {
    let mut cells = Vec::new();
    for line in lines {
        if line.line != target_line {
            continue;
        }
        for run in &line.runs {
            for _ in run.text.chars() {
                cells.push((run.bg.clone(), run.inverse));
            }
        }
    }
    cells
}

fn differing_cells(base: &[(Option<String>, bool)], other: &[(Option<String>, bool)]) -> usize {
    let max_len = base.len().max(other.len());
    (0..max_len)
        .filter(|&i| base.get(i) != other.get(i))
        .count()
}

/// PTY: a scrollback drag whose `Up(Left)` is lost latches the selection; Esc
/// must clear it so later motion can't keep extending it. Drives the real
/// binary: SGR press + drag bytes with no release `m`, then Esc + more motion.
/// Without the recovery guard the post-Esc motion re-extends the latched
/// selection and the final assert fails.
///
/// Real-world lost-Up: xtermjs/xterm.js#4781 (xterm.js — VS Code's terminal —
/// only delivers the mouseup when it lands on the terminal element) and the
/// VS Code tracking issue microsoft/vscode#192518.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn stuck_drag_recovers_on_esc_pty() {
    let content = ContentController::start().await.expect("start content");

    // Hold selections on screen so a wrongly-extended highlight can't flash away before sampling.
    seed_keep_text_selection_config(&content);

    // A wide one-line response gives a long selectable line to drag across.
    let response =
        format!("{MOCK_RESPONSE_SENTINEL} alpha beta gamma delta epsilon zeta eta theta");
    content.set_response(response);

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("response rendered");

    // Focus scrollback so the drag selects the response, not the prompt.
    harness.inject_keys(b"\t").expect("focus scrollback");
    harness.update(Duration::from_millis(400));

    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, MOCK_RESPONSE_SENTINEL)
        .unwrap_or_else(|| panic!("could not locate response line; screen:\n{screen}"));
    let from_col = col;
    let to_col = col + 15;
    let far_col = col + 40;
    // `StyledLine.line` is 1-based; `locate_screen_text` rows are 0-based.
    let target_line = row as usize + 1;

    let base = harness.screen_styled();
    let base_row = row_cells(&base, target_line);

    // Press + drag with no release: the lost-Up the recovery guard must survive.
    harness
        .inject_keys(mouse_drag_no_release(row, from_col, to_col).as_bytes())
        .expect("drag without release");
    harness.update(Duration::from_millis(300));

    let during = harness.screen_styled();
    assert!(
        differing_cells(&base_row, &row_cells(&during, target_line)) > 0,
        "the latched drag should paint a selection highlight; screen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(keys::ESC).expect("esc");
    harness.update(Duration::from_millis(300));

    // The two post-Esc latched-extend paths: `<32` = left-drag motion (button 0 + motion bit),
    // `<35` = bare move with no button held, reported under any-event tracking (DECSET 1003).
    // https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Any-event-tracking
    let far_motion = sgr_mouse(32, row, far_col, 'M') + sgr_mouse(35, row, far_col, 'M').as_str();
    harness
        .inject_keys(far_motion.as_bytes())
        .expect("post-esc motion");
    harness.update(Duration::from_millis(300));

    let after = harness.screen_styled();
    assert_eq!(
        differing_cells(&base_row, &row_cells(&after, target_line)),
        0,
        "Esc must clear the latch so later motion cannot re-extend the selection; screen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager should still be running"
    );

    harness.quit().expect("clean quit");
}
