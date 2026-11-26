// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Mirrors `echo_agent_interjection_core::format::{INTERRUPT_NOTE, UNFINISHED_TASKS_REMINDER}`.
const INTERRUPT_NOTE: &str = "The user interrupted the previous turn:";
const UNFINISHED_TASKS_REMINDER: &str =
    "Make sure to complete any unfinished tasks from previous turns.";

/// Submit OLD, Ctrl+C rewind, send NEW: NEW's request must not contain OLD or interrupt framing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn cancel_then_resend_prompt_appears_once() {
    const OLD_PROMPT: &str = "old prompt that gets yanked";
    const NEW_PROMPT: &str = "brand new question instead";

    let content = ContentController::start().await.expect("start content");
    // Turn 1 is OLD (30s pacing keeps the no-output window); turn 2 is NEW's reply.
    let _rewound_turn =
        content.expect_agent_turn("rewound turn before first token", "GONE never streams.");
    let _resent_turn =
        content.expect_agent_turn("new prompt turn", "RESENT_REPLY to the new prompt.");
    content.set_chunk_delay(Some(Duration::from_secs(30)));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{OLD_PROMPT}\r").as_bytes())
        .expect("submit OLD");
    harness
        .wait_until(
            "OLD block committed and composer cleared",
            Duration::from_secs(30),
            |h| block_lines_containing(h, OLD_PROMPT) == 1 && !composer_holds(h, OLD_PROMPT),
        )
        .expect("OLD block committed");
    harness
        .wait_for_text("Waiting for response", Duration::from_secs(25))
        .expect("turn running pre-first-token");

    harness.inject_keys(keys::CTRL_C).expect("Ctrl+C rewind");
    // Yank then send immediately — no settle window.
    harness
        .wait_until(
            "OLD yanked back into the composer",
            Duration::from_secs(30),
            |h| composer_holds(h, OLD_PROMPT) && block_lines_containing(h, OLD_PROMPT) == 0,
        )
        .expect("OLD restored to composer");

    // Edit the yanked text to NEW and send.
    content.set_chunk_delay(None);
    harness
        .inject_keys(b"\x15")
        .expect("Ctrl+U clears the restored OLD");
    harness
        .inject_keys(format!("{NEW_PROMPT}\r").as_bytes())
        .expect("submit NEW");
    harness
        .wait_for_text("RESENT_REPLY", Duration::from_secs(90))
        .expect("NEW turn reply");

    // NEW once in scrollback; OLD gone.
    harness
        .wait_until(
            "NEW rendered exactly once and OLD gone",
            Duration::from_secs(30),
            |h| {
                block_lines_containing(h, NEW_PROMPT) == 1
                    && !composer_holds(h, NEW_PROMPT)
                    && block_lines_containing(h, OLD_PROMPT) == 0
            },
        )
        .expect("NEW rendered exactly once");

    // NEW's wire request: OLD absent, NEW once, no interrupt framing.
    let mut saw_new_request = false;
    for body in content.request_bodies() {
        let raw = body.to_string();
        if !raw.contains(NEW_PROMPT) {
            continue;
        }
        saw_new_request = true;
        assert!(
            !raw.contains(OLD_PROMPT),
            "the rewound OLD prompt leaked into NEW's request: {body}"
        );
        assert!(
            !raw.contains(INTERRUPT_NOTE),
            "NEW must not be framed as an interrupted follow-up: {body}"
        );
        assert!(
            !raw.contains(UNFINISHED_TASKS_REMINDER),
            "NEW must not carry the unfinished-tasks trailer: {body}"
        );
        // Chat Completions carries `messages`; the Responses shape `input`.
        let items = body["messages"]
            .as_array()
            .or_else(|| body["input"].as_array());
        let users = items
            .into_iter()
            .flatten()
            .filter(|m| {
                m["role"] == "user"
                    && m["content"]
                        .as_str()
                        .is_some_and(|c| c.contains(NEW_PROMPT))
            })
            .count();
        assert_eq!(
            users, 1,
            "NEW must appear in exactly one user message (got {users}): {body}"
        );
    }
    assert!(saw_new_request, "NEW never reached the wire");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
