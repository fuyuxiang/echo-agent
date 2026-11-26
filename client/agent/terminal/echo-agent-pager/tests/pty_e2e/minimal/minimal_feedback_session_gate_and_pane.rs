// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

const FEEDBACK_LABEL_SENTINEL: &str = "How can we improve EchoAgent Build?";
const FEEDBACK_PLACEHOLDER_SENTINEL: &str = "Please provide as much detail as possible.";
const SESSION_GATE_SENTINEL: &str = "No active session";
const THANKS_SENTINEL: &str = "Thanks for the feedback";
const PANE_FEEDBACK: &str = "minimal-pty-feedback-report-xyz";

/// Minimal: bare `/feedback` without a bound session_id shows a system notice;
/// once a session exists the freeform pane opens and submits like full TUI.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn minimal_feedback_session_gate_and_pane() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} minimal ready."));

    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    // Minimal dispatches NewSession at startup; session_id binds when ACP
    // session/new returns. wait_minimal_ready only waits for the idle prompt,
    // so the first `/feedback` can hit the no-session gate or open the pane.
    // Under remote CI a single-shot `/feedback\r` can land Enter before the
    // composer has absorbed the slash filter, so the command never runs and
    // the welcome card still fills the screen after the wait.
    inject_keys_paced(&mut harness, b"/feedback");
    harness.inject_keys(b"\r").expect("submit bare /feedback");
    harness
        .wait_until(
            "session gate notice or feedback pane",
            Duration::from_secs(15),
            |h| {
                h.contains_full_text(SESSION_GATE_SENTINEL)
                    || h.contains_text(FEEDBACK_LABEL_SENTINEL)
            },
        )
        .expect("minimal /feedback must gate or open the pane");

    if !harness.contains_text(FEEDBACK_LABEL_SENTINEL) {
        assert!(
            harness.contains_full_text(SESSION_GATE_SENTINEL),
            "gate must use a system notice (toast is invisible)\nscreen:\n{}",
            harness.screen_contents()
        );
        harness
            .inject_keys(format!("{PROMPT}\r").as_bytes())
            .expect("submit prompt");
        harness
            .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
            .expect("response rendered");
        inject_keys_paced(&mut harness, b"/feedback");
        harness
            .inject_keys(b"\r")
            .expect("submit bare /feedback with session");
        harness
            .wait_for_text(FEEDBACK_LABEL_SENTINEL, Duration::from_secs(15))
            .expect("feedback pane label in minimal");
    }
    assert!(
        harness.contains_text(FEEDBACK_PLACEHOLDER_SENTINEL),
        "composer placeholder must render in minimal\nscreen:\n{}",
        harness.screen_contents()
    );

    harness
        .inject_keys(format!("{PANE_FEEDBACK}\r").as_bytes())
        .expect("submit freeform feedback");
    harness
        .wait_for_full_text(THANKS_SENTINEL, Duration::from_secs(15))
        .expect("minimal pane submit should thank the user");
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
