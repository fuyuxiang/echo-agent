// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Windows twin of `paste_ctrl_v_text_echoes_fast_macos`, exercised by the
/// temporary PR-branch Windows smoke workflow: Ctrl+V (raw 0x16) with plain
/// TEXT on the REAL clipboard must echo in the prompt. The bound is generous
/// (10s) — what matters on this platform is that the paste echoes at all and
/// nothing panics, not the latency figure.
///
/// Skips (loudly) when the session has no usable clipboard — a CI runner
/// without an interactive desktop can't exercise the real paste path.
///
/// WARNING: this test OVERWRITES the machine-global clipboard. The prior TEXT
/// contents are restored best-effort on exit (drop guard, panic included); a
/// prior IMAGE clipboard cannot be restored.
#[cfg(target_os = "windows")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
#[serial_test::serial(host_clipboard)]
async fn paste_ctrl_v_text_echoes_fast_windows() {
    const SENTINEL: &str = "PASTEKEYFASTWWW echo sentinel";

    // Save BEFORE the roundtrip probe: the probe writes a nonce, and a guard
    // taken after it would restore the nonce instead of the user's clipboard.
    let _restore = HostClipboardTextGuard::save();
    if !clipboard_roundtrip_works() {
        eprintln!(
            "SKIP paste_ctrl_v_text_echoes_fast_windows: host clipboard roundtrip failed \
             (no usable clipboard in this session)"
        );
        return;
    }
    pbcopy(SENTINEL).expect("Set-Clipboard sentinel on the host clipboard");

    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Ctrl+V on the welcome screen promotes to a session and re-processes the
    // chord through its prompt, which reads the real host clipboard.
    let start = Instant::now();
    harness.inject_keys(&[0x16]).expect("inject Ctrl+V");
    harness
        .wait_for_text("PASTEKEYFASTWWW", Duration::from_secs(10))
        .expect("clipboard text echoes in the prompt");
    let elapsed = start.elapsed();
    eprintln!(
        "paste_ctrl_v_text_echoes_fast_windows: Ctrl+V text echoed in {} ms",
        elapsed.as_millis()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
