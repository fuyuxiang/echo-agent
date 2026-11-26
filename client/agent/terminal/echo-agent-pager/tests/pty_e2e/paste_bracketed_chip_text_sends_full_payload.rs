// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Bracketed-paste chip guard: a multi-line paste at or above the 4-line chip
/// threshold renders as a compact `[Pasted: N lines]` element instead of
/// inline text, and submitting expands the chip back to the FULL payload —
/// first and last lines included — in the request sent to the model.
///
/// Linux-hermetic (the bracketed clipboard probe is cfg(macos/windows)); on a
/// macOS dev machine an incidental host-clipboard image chip may attach, so
/// asserts are contains-style on unique sentinels only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn paste_bracketed_chip_text_sends_full_payload() {
    const FIRST: &str = "PASTECHIPFIRST leading sentinel line";
    const LAST: &str = "PASTECHIPLAST trailing sentinel line";

    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} chip paste turn."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // 12 lines: sentinel first line, ten filler lines, sentinel last line.
    let mut lines = vec![FIRST.to_owned()];
    for i in 0..10 {
        lines.push(format!("chip filler line {i} keeps the payload multi-line"));
    }
    lines.push(LAST.to_owned());
    let payload = lines.join("\n");

    harness
        .inject_keys(format!("\x1b[200~{payload}\x1b[201~").as_bytes())
        .expect("bracketed-paste 12-line payload");

    // At/above the chip threshold the prompt shows `[Pasted: 12 lines]`.
    harness
        .wait_for_text("Pasted:", Duration::from_secs(5))
        .expect("multi-line paste renders as a Pasted chip");

    harness.inject_keys(b"\r").expect("submit chipped prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("mock response for the chipped paste");

    // The chip must expand to the full pasted text on send.
    let blobs = all_user_message_blobs(&content);
    assert!(
        blobs.iter().any(|m| m.contains(FIRST) && m.contains(LAST)),
        "submitted user message must carry the full chip payload (first + last sentinel lines); got {blobs:?}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
