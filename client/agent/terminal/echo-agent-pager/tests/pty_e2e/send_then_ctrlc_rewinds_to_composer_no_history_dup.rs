// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Ctrl+C BEFORE any server activity rewinds the send: the prompt text
/// returns to the composer AND its scrollback "❯ " block is removed — the UI
/// reads as if the user never hit Send (no stale copy in history, no
/// "Turn cancelled by user" marker). (`do_cancel_turn` rewind path:
/// `set_text` + `remove_entry`; requires `cancel_rewind_enabled`, on by
/// default via the initialize `cancelRewind` meta.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn send_then_ctrlc_rewinds_to_composer_no_history_dup() {
    const REWIND_PROMPT: &str = "rewind me home";

    let content = ContentController::start().await.expect("start content");
    content.set_response("REWIND_NEVER_STREAMS.");
    // "Before any server activity" made deterministic: every SSE event is
    // delayed far beyond the test window, so no chunk can clear the
    // rewindable in-flight stash before Ctrl+C lands.
    content.set_chunk_delay(Some(Duration::from_secs(30)));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{REWIND_PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // The optimistic "❯ " block committed (composer cleared) and the turn is
    // running but pre-first-token — the rewindable window.
    harness
        .wait_until(
            "prompt block committed and composer cleared",
            Duration::from_secs(30),
            |h| block_lines_containing(h, REWIND_PROMPT) == 1 && !composer_holds(h, REWIND_PROMPT),
        )
        .expect("prompt block committed");
    harness
        .wait_for_text("Waiting for response", Duration::from_secs(25))
        .expect("turn running pre-first-token");

    harness.inject_keys(keys::CTRL_C).expect("Ctrl+C rewind");

    // Rewind: text back in the composer, scrollback block gone.
    harness
        .wait_until_stable(
            "rewound prompt restored with its scrollback block removed",
            Duration::from_secs(25),
            Duration::from_millis(500),
            |h| composer_holds(h, REWIND_PROMPT) && block_lines_containing(h, REWIND_PROMPT) == 0,
        )
        .expect("rewound prompt restored");
    // The rewind is silent — it looks like the prompt was never sent.
    assert!(
        !harness.contains_text("Turn cancelled by user"),
        "rewind must not render a cancelled marker\nscreen:\n{}",
        harness.screen_contents()
    );

    // A stalled-stream transport retry of the aborted turn can put the prompt
    // on the wire as two *separate* single-copy requests under slow-runner
    // contention — that is client resilience, not a user-visible duplicate.
    // The real guard is that no *single* request body carries the prompt more
    // than once (a stale rewound copy paired with another send — the 2x bug).
    for body in content.request_bodies() {
        let items = body["messages"]
            .as_array()
            .or_else(|| body["input"].as_array());
        let copies = items
            .into_iter()
            .flatten()
            .filter(|m| {
                m["role"] == "user"
                    && m["content"]
                        .as_str()
                        .is_some_and(|c| c.contains(REWIND_PROMPT))
            })
            .count();
        assert!(
            copies <= 1,
            "rewound prompt must not appear twice in one request (got {copies}): {body}"
        );
    }

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
