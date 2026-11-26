#[allow(unused_imports)]
use super::common::*;

const TAG: &str = "SBGRAB";
const PLAN_LINES: usize = 120;

/// PTY: presses, wheels, and drags on the modal border column next to the
/// scrollbar track must scroll the plan. Users read the thumb + border as
/// one two-column scrollbar and press the border half (reported on macOS
/// Terminal.app and ghostty over SSH), which used to fall into the
/// click-outside-modal path.
///
/// Also pins the thumb contract `bg == fg`: Terminal.app leaves line-gap
/// pixels unpainted under a foreground-only `█`, striping the thumb with
/// dark bars.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn plan_scrollbar_grab_zone_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} first turn done."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--yolo", "--trust", "--no-leader"],
        &[],
        Some(content.home()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness.inject_keys(b"go\r").expect("first turn");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(40))
        .expect("first turn streams");
    // Submitting `exit_plan_mode` before the first turn is idle can consume the
    // scripted tool call while the session is still finalizing; Plan Exit then
    // hangs without parking approval chrome.
    harness
        .wait_for_turn_idle(Duration::from_secs(20))
        .expect("first turn idle");

    let dir = session_dir(&content, &mut harness);
    std::fs::write(dir.join("plan.md"), plan_body(TAG, PLAN_LINES)).expect("seed plan.md");

    let _expectation = expect_tool_turn(&content, "call_plan_sb", "exit_plan_mode", "{}".into());
    harness
        .inject_keys(b"present the plan\r")
        .expect("submit plan prompt");
    harness
        .wait_for_text("Waiting on plan approval", Duration::from_secs(60))
        .unwrap_or_else(|e| {
            panic!(
                "plan approval never parked: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });
    harness
        .wait_until_stable(
            "plan approval preview interactive",
            Duration::from_secs(20),
            Duration::from_millis(250),
            |h| h.contains_text("request changes") && h.contains_text("Waiting on plan approval"),
        )
        .unwrap_or_else(|e| {
            panic!(
                "plan approval preview never settled: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    let before = harness.screen_contents();
    assert!(
        before.contains(&format!("{TAG}000")),
        "viewer must open at the top of the plan; screen:\n{before}"
    );
    let last = format!("{TAG}{:03}", PLAN_LINES - 1);
    assert!(
        !before.contains(&last),
        "the plan tail must start off-screen; screen:\n{before}"
    );

    let (title_row, _) = locate_screen_text(&before, "plan.md").expect("plan viewer title visible");
    let border_col = before
        .lines()
        .nth(title_row as usize)
        .map(|l| l.trim_end().chars().count() as u16 - 1)
        .expect("title row present");
    let (approve_row, _) = locate_screen_text(&before, "approve").expect("footer approve visible");
    let track_bottom_row = approve_row - 2;
    let track_top_row = title_row + 1;

    let track_col = (border_col - 1) as usize;
    let styled = harness.screen_styled();
    let mut thumb_cells = 0;
    for line in &styled {
        let mut col = 0usize;
        for run in &line.runs {
            let run_width = run.text.chars().count();
            if col <= track_col && track_col < col + run_width && run.text.contains('\u{2588}') {
                thumb_cells += 1;
                assert!(run.fg.is_some(), "thumb must have a color (run {run:?})");
                assert_eq!(
                    run.bg, run.fg,
                    "thumb cell background must match the glyph color (run {run:?})"
                );
            }
            col += run_width;
        }
    }
    assert!(thumb_cells > 0, "scrollbar thumb must be visible");

    let mut click = String::new();
    click.push_str(&sgr_mouse(0, track_bottom_row, border_col, 'M'));
    click.push_str(&sgr_mouse(0, track_bottom_row, border_col, 'm'));
    harness.inject_keys(click.as_bytes()).expect("border click");
    harness
        .wait_for_text(&last, Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "border-column click must scroll to the plan tail: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    // A single synthetic notch was observed to be swallowed by the
    // scroll-stream cadence; real wheels emit bursts.
    let wheel_up: String = (0..6)
        .map(|_| sgr_mouse(64, track_bottom_row, border_col, 'M'))
        .collect();
    harness
        .inject_keys(wheel_up.as_bytes())
        .expect("border wheel up");
    harness
        .wait_for_text_absent(&last, Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "wheel-up on the border column must scroll the plan: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    let mut drag = String::new();
    drag.push_str(&sgr_mouse(0, track_bottom_row, border_col, 'M'));
    drag.push_str(&sgr_mouse(
        32,
        (track_top_row + track_bottom_row) / 2,
        border_col,
        'M',
    ));
    drag.push_str(&sgr_mouse(32, track_top_row, border_col, 'M'));
    drag.push_str(&sgr_mouse(0, track_top_row, border_col, 'm'));
    harness.inject_keys(drag.as_bytes()).expect("border drag");
    harness
        .wait_for_text(&format!("{TAG}000"), Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "border-column drag must scroll back to the plan top: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    let after = harness.screen_contents();
    assert!(
        after.contains("request changes"),
        "approval chrome must stay open after scrollbar gestures; screen:\n{after}"
    );
    assert!(
        !after.contains("Type your comment...") && !after.contains("commenting L"),
        "scrollbar gestures must not enter commenting; screen:\n{after}"
    );
    assert!(
        !after.contains("panicked"),
        "pager panicked\nscreen:\n{after}"
    );

    harness.quit().expect("clean quit");
}
