// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// macOS-only, REAL host clipboard: Ctrl+V (raw 0x16) with plain TEXT on the
/// pasteboard must echo fast. The native pasteboard snapshot pre-gate skips
/// the heavy `osascript` attachment probe entirely when no raster is present,
/// so the paste cost is roughly one `pbpaste` read plus a frame.
///
/// WARNING: this test OVERWRITES the machine-global clipboard. The prior TEXT
/// contents are restored best-effort on exit (drop guard, panic included); a
/// prior IMAGE clipboard cannot be restored — `pbpaste` only reads text.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
#[serial_test::serial(host_clipboard)]
async fn paste_ctrl_v_text_echoes_fast_macos() {
    const SENTINEL: &str = "PASTEKEYFASTQQQ echo sentinel";

    let _restore = HostClipboardTextGuard::save();
    pbcopy(SENTINEL).expect("pbcopy sentinel to the host clipboard");

    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Ctrl+V on the welcome screen promotes to a session and re-processes the
    // chord through its prompt, which reads the host clipboard via pbpaste.
    let start = Instant::now();
    harness.inject_keys(&[0x16]).expect("inject Ctrl+V");
    harness
        .wait_for_text("PASTEKEYFASTQQQ", Duration::from_secs(3))
        .expect(
            "clipboard text echoes in the prompt within 3s (the snapshot pre-gate \
             must skip the osascript probe for a raster-less paste)",
        );
    let elapsed = start.elapsed();
    eprintln!(
        "paste_ctrl_v_text_echoes_fast_macos: Ctrl+V text echoed in {} ms",
        elapsed.as_millis()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
