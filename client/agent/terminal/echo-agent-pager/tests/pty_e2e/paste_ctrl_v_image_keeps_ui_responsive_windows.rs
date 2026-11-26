// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Windows twin of `paste_ctrl_v_image_keeps_ui_responsive_macos`, exercised
/// by the temporary PR-branch Windows smoke workflow: Ctrl+V (raw 0x16) with
/// an IMAGE on the REAL clipboard must not block the UI thread. The clipboard
/// read, image decode, and session persist all run off the event loop, so
/// keys typed immediately after the chord echo while the `[Image #N]` chip
/// attaches via a follow-up completion. Ordering is proven by the
/// chip-absence check at echo time (a blocking inline probe attaches the chip
/// before the burst is even processed).
///
/// Skips (loudly) when the session has no usable clipboard — a CI runner
/// without an interactive desktop can't exercise the real paste path.
///
/// WARNING: this test OVERWRITES the machine-global clipboard with an image.
/// The prior TEXT contents are restored best-effort on exit (drop guard); a
/// prior IMAGE clipboard cannot be restored.
#[cfg(target_os = "windows")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
#[serial_test::serial(host_clipboard)]
async fn paste_ctrl_v_image_keeps_ui_responsive_windows() {
    const ECHO: &str = "ZRESPONSIVEZ";

    // Save BEFORE the roundtrip probe: the probe writes a nonce, and a guard
    // taken after it would restore the nonce instead of the user's clipboard.
    let _restore = HostClipboardTextGuard::save();
    if !clipboard_roundtrip_works() {
        eprintln!(
            "SKIP paste_ctrl_v_image_keeps_ui_responsive_windows: host clipboard roundtrip \
             failed (no usable clipboard in this session)"
        );
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir for the clipboard PNG");
    let png = write_fixture_png(tmp.path()).expect("write clipboard fixture png");
    set_clipboard_png(&png).expect("PowerShell SetImage clipboard PNG");

    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Ctrl+V then a typed burst in ONE injected buffer: the burst chases the
    // chord with no settle, so it can only echo promptly if the paste's
    // clipboard read/decode/persist is off the event loop.
    let start = Instant::now();
    let mut keys = vec![0x16];
    keys.extend_from_slice(ECHO.as_bytes());
    harness
        .inject_keys(&keys)
        .expect("inject Ctrl+V + typed burst");

    harness
        .wait_for_text(ECHO, Duration::from_secs(10))
        .expect("typed burst echoes while the paste probe runs off-thread");
    let echo_elapsed = start.elapsed();

    // Ordering guard: the chip must NOT already be on screen when the burst
    // first echoes — an inline (blocking) probe attaches it before the burst.
    assert!(
        !harness.contains_text("Image #"),
        "image chip attached before the typed burst echoed — the clipboard \
         read/persist blocked the UI thread\nscreen:\n{}",
        harness.screen_contents()
    );

    harness
        .wait_for_text("Image #1", Duration::from_secs(30))
        .expect("deferred clipboard probe attaches the [Image #1] chip");
    let chip_elapsed = start.elapsed();

    eprintln!(
        "paste_ctrl_v_image_keeps_ui_responsive_windows: typed-burst echo {} ms, image chip {} ms",
        echo_elapsed.as_millis(),
        chip_elapsed.as_millis()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
