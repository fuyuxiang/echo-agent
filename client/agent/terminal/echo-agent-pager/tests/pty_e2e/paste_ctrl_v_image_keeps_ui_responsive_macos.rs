// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// macOS-only, REAL host clipboard: Ctrl+V (raw 0x16) with an IMAGE on the
/// pasteboard must not block the UI thread. The clipboard read, image decode,
/// and session persist all run off the event loop, so keys typed immediately
/// after the chord echo right away while the `[Image #N]` chip attaches via a
/// follow-up completion.
///
/// This FAILS on a pre-offthread pager: the probe ran inline on the event
/// loop, so the chip attached BEFORE the typed burst was even processed (and
/// the burst echo stalled behind the ~0.5-1s osascript read + persist).
///
/// WARNING: this test OVERWRITES the machine-global clipboard with an image.
/// The prior TEXT contents are restored best-effort on exit (drop guard); a
/// prior IMAGE clipboard cannot be restored — `pbpaste` only reads text.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
#[serial_test::serial(host_clipboard)]
async fn paste_ctrl_v_image_keeps_ui_responsive_macos() {
    const ECHO: &str = "ZRESPONSIVEZ";

    let _restore = HostClipboardTextGuard::save();

    let tmp = tempfile::tempdir().expect("tempdir for the clipboard PNG");
    let png = write_fixture_png(tmp.path()).expect("write clipboard fixture png");
    set_clipboard_png(&png).expect("osascript set clipboard PNG");

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
        .wait_for_text(ECHO, Duration::from_secs(2))
        .expect("typed burst echoes within 2s while the paste probe runs off-thread");
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
        .wait_for_text("Image #1", Duration::from_secs(20))
        .expect("deferred clipboard probe attaches the [Image #1] chip");
    let chip_elapsed = start.elapsed();

    // Ordering is proven by the chip-absence check above (screen state at echo
    // time); comparing the two sequenced elapsed() readings would be vacuous.
    eprintln!(
        "paste_ctrl_v_image_keeps_ui_responsive_macos: typed-burst echo {} ms, image chip {} ms",
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
