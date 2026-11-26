// Per-test-case module for the `pty_e2e` integration test crate.
//
// Drag-select and copy out of a prompt pinned as a sticky header (painted outside the content renderer's selection bookkeeping).
#[allow(unused_imports)]
use super::common::*;
use echo_agent_pager_pty_harness::StyledLine;

/// 60 answer lines over a 50-row terminal overflow the viewport, so the prompt pins to the top.
const LAST_LINE: &str = "ANSWERLINE060";

/// Only rendered in a pinned header's gap row; the test's pinned-ness proof.
const UP_INDICATOR: &str = "▲";

/// Unique single-row prompt ([`PROMPT`] "go" collides with footer copy).
const STICKY_PROMPT: &str = "STICKYCOPY alpha bravo charlie delta";

/// Substring the clipboard must contain after the drag.
const EXPECTED_COPY: &str = "STICKYCOPY alpha bravo";

fn long_answer() -> String {
    (1..=60)
        .map(|i| format!("- ANSWERLINE{i:03} lorem ipsum dolor sit amet consectetur"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Per-cell `(bg, inverse)` for one row (`StyledLine.line` is 1-based, `row` 0-based); the selection overlay shows up as a change in these.
fn row_cells(lines: &[StyledLine], row: u16) -> Vec<(Option<String>, bool)> {
    let target = row as usize + 1;
    let mut cells = Vec::new();
    for line in lines {
        if line.line != target {
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

/// Dragging across a pinned sticky header highlights and copies the dragged text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn sticky_header_drag_copy_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(long_answer());

    let binary = pager_binary().expect("resolve pager binary");
    // SSH_CONNECTION forces the OSC 52 clipboard route.
    let env_refs: Vec<(&str, &str)> = vec![("SSH_CONNECTION", "scripted-test 1 127.0.0.1 2")];
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
        .inject_keys(format!("{STICKY_PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // Follow mode parks at the answer's tail, pinning the prompt on top.
    harness
        .wait_for_text(LAST_LINE, Duration::from_secs(60))
        .expect("answer tail visible (follow mode at the bottom)");
    harness
        .wait_for_text(UP_INDICATOR, Duration::from_secs(10))
        .expect("▲ indicator rendered under the sticky prompt header");

    // Focus scrollback, then let the turn-end relayout settle.
    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(10))
        .expect("scrollback focused (Space:prompt hint) after Tab");
    harness.update(Duration::from_millis(1500));

    let screen = harness.screen_contents();
    let (prompt_row, prompt_col) = locate_screen_text(&screen, "STICKYCOPY").unwrap_or_else(|| {
        panic!("could not locate the pinned prompt; screen:\n{screen}");
    });
    let (arrow_row, _) = locate_screen_text(&screen, UP_INDICATOR).expect("locate ▲ on screen");
    // Precondition: the prompt is the pinned header, not an inline one.
    assert!(
        prompt_row < arrow_row && arrow_row < prompt_row + 6,
        "prompt at row {prompt_row} should be pinned above the ▲ at row \
         {arrow_row}\nscreen:\n{screen}"
    );

    let baseline = row_cells(&harness.screen_styled(), prompt_row);

    // Hold a drag through the end of "bravo" to inspect the live overlay.
    let end_col = prompt_col + EXPECTED_COPY.chars().count() as u16 - 1;
    let mut held = String::new();
    held.push_str(&sgr_mouse(0, prompt_row, prompt_col, 'M'));
    held.push_str(&sgr_mouse(32, prompt_row, (prompt_col + end_col) / 2, 'M'));
    held.push_str(&sgr_mouse(32, prompt_row, end_col, 'M'));
    harness
        .inject_keys(held.as_bytes())
        .expect("drag across the pinned prompt (held)");

    // Poll for the live overlay (cheap when healthy, tolerant under CI load).
    let mut during = Vec::new();
    for _ in 0..20 {
        harness.update(Duration::from_millis(150));
        during = row_cells(&harness.screen_styled(), prompt_row);
        if during != baseline {
            break;
        }
    }
    // Release first: a held pointer here is what used to arm autoscroll.
    harness
        .inject_keys(sgr_mouse(0, prompt_row, end_col, 'm').as_bytes())
        .expect("release drag");

    assert_ne!(
        during,
        baseline,
        "the held drag should paint a selection highlight on the pinned \
         header row; screen:\n{}",
        harness.screen_contents()
    );
    assert_ne!(
        during.get(prompt_col as usize),
        baseline.get(prompt_col as usize),
        "the first dragged cell should carry the selection highlight"
    );
    // Negative control: cells past `end_col` keep their baseline styling, so a whole-row restyle cannot satisfy the assertion above.
    let control_col = (end_col + 3) as usize;
    assert_eq!(
        during.get(control_col),
        baseline.get(control_col),
        "cells past the drag's end column must not be highlighted"
    );

    // Still pinned: this gesture used to autoscroll the transcript away.
    assert!(
        harness.contains_text(UP_INDICATOR),
        "the pinned header must survive the drag (no autoscroll); screen:\n{}",
        harness.screen_contents()
    );
    let after_drag = harness.screen_contents();
    let (still_pinned_row, _) =
        locate_screen_text(&after_drag, "STICKYCOPY").unwrap_or_else(|| {
            panic!("the pinned prompt vanished during the drag; screen:\n{after_drag}");
        });
    assert_eq!(
        still_pinned_row, prompt_row,
        "the pinned prompt must not move during the drag; screen:\n{after_drag}"
    );

    let payloads = wait_for_osc52_payloads(&mut harness, Duration::from_secs(10));
    assert!(
        !payloads.is_empty(),
        "expected an OSC 52 clipboard write from the sticky-header drag; \
         screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    assert!(
        joined.contains(EXPECTED_COPY),
        "clipboard should contain the dragged prompt text {EXPECTED_COPY:?}; \
         payloads={payloads:?}"
    );
    assert!(
        !joined.contains('❯'),
        "clipboard must not contain the prompt arrow chrome; payloads={payloads:?}"
    );
    // Upper bound: the copy must stop where the drag did.
    assert!(
        !joined.contains("charlie"),
        "the copy must stop at the drag's end column; payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
