// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Teardown must not gate `MOUSE_PASTE_RESET` on `MOUSE_CAPTURE_ENABLED`.
/// Minimal never captures the mouse, so that flag is always false there, yet it
/// enables bracketed paste like every other mode and `?2004l` rides in the same
/// write — gating left `?2004` on in the user's shell after every clean minimal
/// exit. Minimal is the only mode where the flag is false by design, making this
/// the one such window reachable without fault injection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_quit_resets_bracketed_paste() {
    const RESET: &str = "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2004l";

    let content = ContentController::start().await.expect("start content");
    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    // Ctrl+C arms the quit confirmation, the second exits.
    harness.inject_keys(b"\x03").expect("inject Ctrl+C");
    harness
        .wait_for_text("again to quit", Duration::from_secs(5))
        .expect("quit confirmation");
    harness.inject_keys(b"\x03").expect("inject Ctrl+C again");
    let _ = harness.wait_exit_code(Duration::from_secs(10));
    for _ in 0..10 {
        harness.update(Duration::from_millis(100));
    }

    let raw = String::from_utf8_lossy(harness.raw_output()).into_owned();
    let enable = raw
        .rfind("\x1b[?2004h")
        .expect("minimal must enable bracketed paste");
    assert!(
        raw.rfind(RESET).is_some_and(|reset| reset > enable),
        "minimal teardown must reset bracketed paste even though it never \
         captures the mouse\nraw:\n{raw:?}"
    );
}
