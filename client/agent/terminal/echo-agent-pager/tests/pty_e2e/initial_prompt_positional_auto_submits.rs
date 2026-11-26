// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 11. **Initial prompt positional.**
/// Launching `echoagent "<prompt>"` (the prompt passed as a positional CLI arg)
/// auto-starts a new session and submits the prompt as the first turn —
/// no keystrokes injected. This exercises the full loop end-to-end:
/// CLI positional → TUI launch → NewSession → SendPrompt → shell agent →
/// mock inference → streamed chunks → pager render.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn initial_prompt_positional_auto_submits() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} hello from the auto-submitted initial prompt."
    ));

    let binary = pager_binary().expect("resolve pager binary");
    // Pass the prompt as a positional argument, exactly like `echoagent "go"`.
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[PROMPT])
            .expect("spawn pager with initial prompt");

    // No keys are injected: the positional prompt must auto-run and the
    // mock response must appear on screen on its own.
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("mock response from auto-submitted initial prompt");
    assert!(
        content.has_chat_completion(),
        "mock inference server never received a chat completion request\nrequests: {:?}",
        content.requests()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
