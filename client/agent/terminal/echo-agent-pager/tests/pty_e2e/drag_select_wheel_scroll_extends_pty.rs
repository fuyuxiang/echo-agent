// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

/// Tall enough that a mid-drag wheel burst has ~150 rows of headroom above
/// the bottom-pinned viewport.
const MARKER_COUNT: usize = 240;

/// Wheel-up reports sent mid-drag in one write (batched with the follow-up
/// one-cell motion so no redraw separates them).
const WHEEL_NOTCHES: usize = 16;

/// The extension witness: a marker this many lines above the pre-wheel head.
/// 16 notches scroll ≥16 rows under any stream classification, so the
/// post-wheel head is at least this far up; the pre-wheel span was 2 rows.
const MIN_EXTEND_LINES: usize = 12;

/// PTY: wheel-scrolling mid-drag extends the selection — the next draw
/// rebuilds the model and re-snaps the head to the held pointer, so lines
/// revealed by the wheel land in the copy.
///
/// The wheel reports and the one-cell motion ride ONE write so the motion
/// resolves against the pre-wheel model; only the post-render reclamp can
/// extend the head to the revealed rows. `SSH_CONNECTION` forces the OSC 52
/// clipboard route for readback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn drag_select_wheel_scroll_extends_pty() {
    let (mut harness, _content, top_before) = spawn_bottom_pinned_marker_scrollback_with_env(
        MARKER_COUNT,
        &[("SSH_CONNECTION", "scripted-test 1 127.0.0.1 2")],
    )
    .await;

    // Anchor ~25 rows below the topmost visible marker: mid-screen, clear of
    // both autoscroll edge zones.
    let anchor_idx = top_before + 25;
    let anchor_marker = marker_line(anchor_idx);
    let screen = harness.screen_contents();
    let (row_a, col_a) = locate_screen_text(&screen, &anchor_marker)
        .unwrap_or_else(|| panic!("could not locate {anchor_marker:?}; screen:\n{screen}"));

    // Press at the END of the anchor marker (the anchor line is the span's
    // bottom endpoint; its slice runs 0..=press col, keeping the token whole),
    // then drag two rows UP ending at the marker's start column.
    let press_col = col_a + anchor_marker.len() as u16 - 1;
    let mut drag = String::new();
    drag.push_str(&sgr_mouse(0, row_a, press_col, 'M'));
    drag.push_str(&sgr_mouse(32, row_a - 1, col_a + 5, 'M'));
    drag.push_str(&sgr_mouse(32, row_a - 2, col_a, 'M'));
    harness
        .inject_keys(drag.as_bytes())
        .expect("press and drag up");
    harness.update(Duration::from_millis(400));

    // Wheel up mid-drag, then move one cell left — batched into one write.
    let mut wheel = String::new();
    for _ in 0..WHEEL_NOTCHES {
        wheel.push_str(&sgr_mouse(SGR_SCROLL_UP, row_a - 2, col_a, 'M'));
    }
    wheel.push_str(&sgr_mouse(32, row_a - 2, col_a.saturating_sub(1), 'M'));
    harness
        .inject_keys(wheel.as_bytes())
        .expect("wheel burst mid-drag");
    harness.update(Duration::from_millis(800));

    harness
        .inject_keys(sgr_mouse(0, row_a - 2, col_a.saturating_sub(1), 'm').as_bytes())
        .expect("release");

    let payloads = wait_for_osc52_payloads(&mut harness, Duration::from_secs(10));
    assert!(
        !payloads.is_empty(),
        "expected an OSC 52 clipboard write after release; screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    assert!(
        joined.contains(&anchor_marker),
        "clipboard must contain the anchor line; payloads={payloads:?}"
    );
    let revealed_marker = marker_line(anchor_idx - MIN_EXTEND_LINES);
    assert!(
        joined.contains(&revealed_marker),
        "clipboard must span rows revealed by the mid-drag wheel scroll \
         (expected {revealed_marker}); payloads={payloads:?}"
    );
    assert!(
        !joined.contains(&marker_line(anchor_idx + 1)),
        "an upward drag must not copy below the anchor line; payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
