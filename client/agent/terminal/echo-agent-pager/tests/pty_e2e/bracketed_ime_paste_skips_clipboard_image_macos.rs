// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Regression, macOS-only, REAL host pasteboard — the exact reported
/// surface: under Otty (`TERM_PROGRAM=otty`, the only terminal known to
/// deliver macOS IME commits as bracketed paste), with an image on the
/// clipboard, an IME commit must not attach that image to the agent prompt.
///
/// Skips (loudly) when the session has no usable clipboard — a CI runner
/// without a pasteboard shouldn't fail on environment.
///
/// WARNING: this test OVERWRITES the machine-global clipboard with an image.
/// A prior TEXT clipboard is restored on exit; a prior IMAGE cannot be.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
#[serial_test::serial(host_clipboard)]
async fn bracketed_ime_paste_skips_clipboard_image_macos() {
    use echo_agent_pager_pty_harness::host_clipboard::{
        HostClipboardTextGuard, clipboard_roundtrip_works, set_clipboard_png, write_fixture_png,
    };

    const IME_PAYLOAD: &str = "中文";

    // Guard FIRST: the roundtrip check overwrites the clipboard, and a guard
    // taken after it would restore the nonce instead of the user's clipboard.
    let _restore = HostClipboardTextGuard::save();
    if !clipboard_roundtrip_works() {
        eprintln!(
            "SKIP bracketed_ime_paste_skips_clipboard_image_macos: host clipboard \
             roundtrip failed (no usable clipboard in this session)"
        );
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir for the clipboard PNG");
    let png = write_fixture_png(tmp.path()).expect("write clipboard png fixture");
    set_clipboard_png(&png).expect("put the fixture PNG on the host pasteboard");

    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    // The payload-origin gate only runs under Otty (TERM_PROGRAM=otty).
    let mut harness = PtyHarness::spawn_with_content_env_ops(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &[EnvOp::set("TERM_PROGRAM", "otty")],
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    harness
        .inject_keys(format!("\x1b[200~{IME_PAYLOAD}\x1b[201~").as_bytes())
        .expect("bracketed IME payload");

    harness
        .wait_for_text(IME_PAYLOAD, Duration::from_secs(10))
        .expect("IME text echoes in the agent prompt");

    // Settle long enough for the deferred clipboard probe to resolve.
    harness.update(Duration::from_secs(3));
    assert!(
        !harness.contains_text("[Image #"),
        "IME-committed text must NOT attach the clipboard image\nscreen:\n{}",
        harness.screen_contents()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
