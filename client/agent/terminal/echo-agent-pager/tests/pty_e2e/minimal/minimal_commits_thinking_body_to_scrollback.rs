// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;
use echo_agent_pager_pty_harness::{InferenceEndpoint, InferenceRequestMatcher};

/// Reasoning text streamed by the mock. Must never appear in the answer text
/// so screen assertions can tell the two apart.
const REASONING_SENTINEL: &str = "REASONINGSENTINEL";

/// `[ui] show_thinking_blocks` is set explicitly (not left to the default)
/// so the test doesn't depend on the rollout default.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_commits_thinking_body_to_scrollback() {
    // The model must run on the Responses backend — reasoning summary deltas
    // are a Responses-API stream shape (the scripted events below).
    let content = ContentController::start_with_models(vec![
        MockModel::new("test-model").with_api_backend("responses"),
    ])
    .await
    .expect("start content");
    let reasoning = format!("{REASONING_SENTINEL} pondering syllables quietly");
    let answer = format!("{MOCK_RESPONSE_SENTINEL} the answer body.");
    let _thinking_turn = content.expect_response(
        "minimal transcript reasoning turn",
        InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
        ScriptedResponse::sse(sse::responses_api_reasoning_and_text_events(
            &reasoning,
            &answer,
            "test-model",
        )),
    );
    // Fallback mode for any further auxiliary traffic.
    content.set_response(answer.clone());

    // Thinking blocks explicitly ON (ingestion is gated on this toggle; the
    // sandbox `$HOME` starts with no config at all).
    std::fs::create_dir_all(content.home().join(".echo-agent")).expect("mk .echo-agent");
    std::fs::write(
        content.home().join(".echo-agent/config.toml"),
        "[ui]\nshow_thinking_blocks = true\n",
    )
    .expect("write config");

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, MINIMAL_ARGS)
            .expect("spawn minimal pager");
    harness.set_respond_to_queries(true);

    wait_minimal_ready(&mut harness);

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_full_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn committed");

    harness
        .wait_for_full_text("Thought for", Duration::from_secs(10))
        .expect("thinking header committed");
    harness
        .wait_for_full_text(REASONING_SENTINEL, Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "reasoning body must be committed to scrollback: {e}\nfull:\n{}",
                harness.full_text()
            )
        });
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
