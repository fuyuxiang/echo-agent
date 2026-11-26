// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Minimal + Apple Terminal: Ctrl+O is the send-now chord. With an empty
/// composer and a mid-turn queued follow-up it must send that row now —
/// cancel-and-send: turn 1 is cancelled silently and the row runs as its own
/// next turn (with the interjection preamble) — not open the transcript pager remap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_ctrl_o_send_now_queued_apple_terminal() {
    let content = ContentController::start().await.expect("start content");
    let mut turn_one = content.expect_agent_turn_blocked(
        "running turn before minimal Ctrl+O send-now",
        slow_turn_text("STEPONE"),
    );
    let _turn_two = content.expect_agent_turn(
        "minimal Ctrl+O sent-now prompt",
        "STEPTWO send-now via Ctrl+O acknowledged.",
    );

    let binary = pager_binary().expect("resolve pager binary");
    let mut overrides: Vec<(String, String)> =
        vec![("TERM_PROGRAM".into(), "Apple_Terminal".into())];
    // Non-interactive $PAGER so a mistaken transcript open fails fast rather
    // than hanging in `less` if the predicate regresses.
    overrides.push(("PAGER".into(), "cat".into()));
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        MINIMAL_ARGS,
        &env_refs,
    )
    .expect("spawn minimal + Apple_Terminal");
    harness.set_respond_to_queries(true);

    wait_minimal_ready(&mut harness);

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text("STEPONE", Duration::from_secs(30))
        .expect("turn 1 streaming");
    tokio::time::timeout(Duration::from_secs(10), turn_one.wait_blocked())
        .await
        .expect("turn 1 reached completion barrier");

    harness
        .inject_keys(b"minimal send-now payload\r")
        .expect("queue follow-up");
    harness
        .wait_for_text("1 queued", Duration::from_secs(10))
        .expect("queue indicator");

    // Empty composer + queue: Ctrl+O must yield to send-now, not transcript.
    // Cancel-and-send: the shell silently cancels turn 1 (its held completion
    // is irrelevant — the abort wins) and the row commits as a standard "❯ "
    // prompt block for its own turn. Turn 1 is still gated open here, so the
    // queued row cannot have promoted FIFO.
    harness.inject_keys(CTRL_O).expect("Ctrl+O send-now");
    // Generous deadline: with turn 1 gated open there is no promotion race
    // left to mask — this wait is pure render latency, which under heavy
    // parallel-suite load can exceed the old 15s budget.
    harness
        .wait_for_text("\u{276F} minimal send-now payload", Duration::from_secs(60))
        .expect("send-now chrome (not a silent transcript open)");

    // Let the mock's gate go so the promoted turn streams its reply.
    turn_one.release();
    harness
        .wait_for_text("STEPTWO", Duration::from_secs(40))
        .expect("send-now turn reply");

    // The send-now cancel of turn 1 is silent (scrollback-aware check:
    // minimal commits blocks into native history).
    assert!(
        !harness.contains_full_text("Turn cancelled by user"),
        "send-now cancel must not render a cancelled marker\nfull contents:\n{}",
        harness.full_text()
    );

    let users = all_user_message_blobs(&content);
    let sent = users
        .iter()
        .find(|u| u.contains("minimal send-now payload"))
        .unwrap_or_else(|| panic!("queued follow-up never on wire: {users:#?}"));
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
    quit_minimal(&mut harness);
}
