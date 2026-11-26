// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 19b. **VS Code family: Ctrl+L (form feed) is the send-now chord** with the
/// same cancel-and-send semantics as the default Ctrl+Enter binding: the
/// running turn is cancelled silently and the composer text runs as its own
/// next turn (with the interjection preamble).
/// Harness strips `TERM_PROGRAM` then applies env — pass `vscode` so
/// defaults bind the chord to Ctrl+L.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn interjection_reaches_model_ctrl_l_in_vscode_family() {
    let content = ContentController::start().await.expect("start content");
    // Gate turn 1's terminal event so the typed text + chord provably land
    // mid-turn regardless of suite load.
    let mut turn_one = content.expect_agent_turn_blocked(
        "running turn before VS Code send-now",
        slow_turn_text("TURNONE"),
    );
    let _turn_two = content.expect_agent_turn(
        "VS Code sent-now message",
        "TURNTWO reply to the sent-now message.",
    );

    let binary = pager_binary().expect("resolve pager binary");
    let overrides: Vec<(String, String)> = vec![("TERM_PROGRAM".into(), "vscode".into())];
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &env_refs,
    )
    .expect("spawn pager with vscode brand");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text("TURNONE", Duration::from_secs(30))
        .expect("turn 1 streaming");
    tokio::time::timeout(Duration::from_secs(10), turn_one.wait_blocked())
        .await
        .expect("turn 1 reached completion barrier");

    harness
        .inject_keys(b"please also check the logs")
        .expect("type message");
    harness.inject_keys(CTRL_L).expect("send-now via Ctrl+L");
    turn_one.release();
    harness
        .wait_for_text(
            "\u{276F} please also check the logs",
            Duration::from_secs(15),
        )
        .expect("send-now prompt block");
    harness
        .wait_for_text("TURNTWO", Duration::from_secs(40))
        .expect("sent-now message ran as the next turn");

    // The send-now cancel of turn 1 is silent.
    assert!(
        !harness.contains_text("Turn cancelled by user"),
        "send-now cancel must not render a cancelled marker\nscreen:\n{}",
        harness.screen_contents()
    );

    let users = all_user_messages(&content);
    let sent = users
        .iter()
        .find(|u| u.contains("please also check the logs"))
        .unwrap_or_else(|| panic!("sent-now message never reached the wire: {users:#?}"));
    assert!(
        sent.contains(INTERJECTION_WIRE_PREFIX),
        "send-now must use the interjection preamble: {sent}"
    );
    assert!(
        sent.contains("<user_query>"),
        "send-now must arrive as a standard user_query prompt: {sent}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
