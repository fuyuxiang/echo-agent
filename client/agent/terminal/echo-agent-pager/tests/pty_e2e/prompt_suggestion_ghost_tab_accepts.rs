// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// The suggestion the mock streams for every request. Serves BOTH the prompt
/// turn and the turn-end `echo.agent/suggestPrompt` call (the mock's fixed mode
/// answers every request), so after the turn the ghost text mirrors it.
/// Multi-word so the shell-side sanitizer (`sanitize_suggestion`) accepts it.
const SUGGESTION: &str = "review the staged changes";

/// Bottom-bar hint that only renders while the prompt-suggestion ghost is
/// visible (`build_hints` — `ActivePane::Prompt` arm). The ghost text itself
/// is indistinguishable from the agent's reply in a plain-text screen dump
/// (same string), so the hint is the observable for ghost visibility.
const ACCEPT_HINT: &str = "accept suggestion";

/// Next-prompt autocomplete e2e: after a turn completes, the predicted next
/// prompt renders as ghost text in the empty prompt with a bottom-bar
/// "accept suggestion" hint; typing a non-matching char hides it; clearing
/// the input brings it back; Tab accepts it into the prompt (hint gone, text
/// editable — verified by extending it and observing the echo).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn prompt_suggestion_ghost_tab_accepts() {
    // The suggestion model (`echo-agent-build-0.1`, the shell's built-in default)
    // must be in the mock catalog: the shell catalog-guards the effective
    // suggestion model and *skips the request entirely* when it is not
    // sampleable (`prompt_suggest::effective_suggest_model`), exactly as it
    // does for OAuth users whose catalogs exclude it. Listing it exercises
    // the real guarded path end-to-end: pager hints the model from its
    // catalog → shell guard passes → request fires → ghost renders.
    // `test-model` stays first so it remains the session's default model.
    let content = ContentController::start_with_models(vec![
        MockModel::new("test-model"),
        MockModel::new("echo-agent-build-0.1"),
    ])
    .await
    .expect("start content");
    content.set_response(SUGGESTION);

    // The sandbox baseline disables the feature for the suite; re-enable it here.
    let overrides = [(
        "ECHO_AGENT_PROMPT_SUGGESTIONS".to_owned(),
        "true".to_owned(),
    )];
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &env_refs,
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(SUGGESTION, Duration::from_secs(30))
        .expect("agent reply on screen");

    // Turn end fires the suggestion fetch; the ghost + hint follow.
    harness
        .wait_for_text(ACCEPT_HINT, Duration::from_secs(20))
        .expect("prompt-suggestion ghost visible (accept hint in shortcuts bar)");

    // A non-matching keystroke hides the ghost (and its hint)…
    harness.inject_keys(b"x").expect("type non-matching char");
    wait_for_text_gone(&mut harness, ACCEPT_HINT, Duration::from_secs(5))
        .expect("ghost hidden after divergent typing");

    // …and clearing the input brings the suggestion back.
    harness.inject_keys(b"\x7f").expect("backspace");
    harness
        .wait_for_text(ACCEPT_HINT, Duration::from_secs(5))
        .expect("ghost returns once the input is empty again");

    // Tab accepts: the hint goes away and the suggestion is now real,
    // editable prompt text — typing appends after it (echo proves the
    // cursor sits at the end of the accepted text).
    harness.inject_keys(b"\t").expect("tab accepts suggestion");
    wait_for_text_gone(&mut harness, ACCEPT_HINT, Duration::from_secs(5))
        .expect("hint gone after accept");
    harness
        .inject_keys(b" and the tests")
        .expect("extend accepted prompt");
    harness
        .wait_for_text(
            "review the staged changes and the tests",
            Duration::from_secs(5),
        )
        .expect("accepted suggestion is editable prompt text");

    harness.quit().expect("clean quit");
}

/// Poll until `text` is absent from the screen (inverse of `wait_for_text`).
fn wait_for_text_gone(
    harness: &mut PtyHarness,
    text: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        harness.update(Duration::from_millis(50));
        if !harness.contains_text(text) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out after {timeout:?} waiting for text to disappear: {text:?}\nscreen:\n{}",
                harness.screen_contents()
            );
        }
    }
}
