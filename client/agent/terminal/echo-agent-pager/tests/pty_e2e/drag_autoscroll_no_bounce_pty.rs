// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

/// PTY: drag-autoscroll must not bounce. Over tall content scrolled off the
/// bottom, a drag held past the pane's bottom edge autoscrolls down; the
/// topmost visible marker, sampled every ~100ms, must be monotonically
/// non-decreasing (never a regression to an earlier marker = no direction
/// flip, no offset jitter) and must settle back at the bottom clamp.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn drag_autoscroll_no_bounce_pty() {
    let (mut harness, _content, baseline_topmost) = spawn_bottom_pinned_marker_scrollback(60).await;

    // Wheel up (back-to-back reports classify as wheel) so the autoscroll
    // has room to move back down to the clamp.
    let mut wheel = String::new();
    for _ in 0..12 {
        wheel.push_str(&sgr_mouse(SGR_SCROLL_UP, WHEEL_ROW, WHEEL_COL, 'M'));
    }
    harness.inject_keys(wheel.as_bytes()).expect("wheel up");
    harness.update(Duration::from_millis(600));
    let scrolled_topmost = topmost_visible_marker(&harness).expect("markers visible");
    assert!(
        scrolled_topmost + 3 <= baseline_topmost,
        "setup: wheel-up must scroll up (topmost {scrolled_topmost} vs baseline {baseline_topmost})\nscreen:\n{}",
        harness.screen_contents()
    );

    // Press on a visible marker line (text anchor), then drag to the strip
    // row above the prompt box — past the pane's bottom edge — and HOLD.
    let screen = harness.screen_contents();
    let marker_text = format!("MARKER-{:04}", scrolled_topmost + 3);
    let (press_row, press_col) = locate_screen_text(&screen, &marker_text)
        .unwrap_or_else(|| panic!("could not locate {marker_text:?}; screen:\n{screen}"));
    let (placeholder_row, _) = locate_screen_text(&screen, "Build anything")
        .unwrap_or_else(|| panic!("could not locate the prompt placeholder; screen:\n{screen}"));
    let hold_row = placeholder_row - 2;
    assert!(hold_row > press_row, "setup: hold point below the press");

    // Two motion samples, as any real drag emits: the first promotes the
    // pending drag, the second (at the held position) arms the autoscroll —
    // promotion itself deliberately arms nothing.
    let mut drag = String::new();
    drag.push_str(&sgr_mouse(0, press_row, press_col, 'M'));
    drag.push_str(&sgr_mouse(32, press_row + 1, press_col, 'M'));
    drag.push_str(&sgr_mouse(32, hold_row, press_col, 'M'));
    harness
        .inject_keys(drag.as_bytes())
        .expect("press a marker, drag past the bottom edge");

    // Sample the viewport every ~100ms with the pointer held: the topmost
    // marker must never regress (bounce) and must settle at the clamp.
    let mut samples = Vec::new();
    for _ in 0..25 {
        harness.update(Duration::from_millis(100));
        samples.push(topmost_visible_marker(&harness).unwrap_or_else(|| {
            panic!(
                "markers must stay visible mid-autoscroll\nscreen:\n{}",
                harness.screen_contents()
            )
        }));
    }
    for pair in samples.windows(2) {
        assert!(
            pair[1] >= pair[0],
            "viewport bounced: topmost regressed {} -> {}; samples={samples:?}",
            pair[0],
            pair[1]
        );
    }
    let last = *samples.last().unwrap();
    assert!(
        last > samples[0] || samples[0] == baseline_topmost,
        "autoscroll made no progress; samples={samples:?}"
    );
    assert_eq!(
        last, baseline_topmost,
        "must settle back at the bottom clamp; samples={samples:?}"
    );
    assert!(
        samples[samples.len() - 5..].iter().all(|&m| m == last),
        "must hold flat once clamped (no jitter); samples={samples:?}"
    );

    harness
        .inject_keys(sgr_mouse(0, hold_row, press_col, 'm').as_bytes())
        .expect("release");
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
