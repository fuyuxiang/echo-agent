// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Minimal mode paints the welcome card edge-to-edge (no outer horizontal pad).
/// The live region's status / prompt / info rows and committed user+agent blocks
/// must share that left edge — previously they sat at `block_pad_left + accent`
/// (= 3 columns of blank gutter), which looked misaligned against the welcome
/// box. Assert every non-blank visible row either is welcome-card chrome
/// (border / logo interior) or starts at column 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_flush_left_no_hpad() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} — flush-left alignment check."
    ));

    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    // Idle: status + prompt + info bar must be flush-left (no leading spaces).
    assert_flush_left_live_rows(
        &harness.screen_contents(),
        &["minimal · /help"],
        "idle live region",
    );

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("response renders");
    // Let the turn finish and commit settle into scrollback / live region.
    harness
        .wait_for_text(MINIMAL_IDLE_SENTINEL, Duration::from_secs(20))
        .expect("return to idle after response");
    harness.update(Duration::from_millis(200));

    let screen = harness.screen_contents();
    assert!(
        screen.contains(MOCK_RESPONSE_SENTINEL),
        "response must be on screen\nscreen:\n{screen}"
    );
    // Committed user line (`❯ go`), agent response, and live chrome all
    // flush-left with the welcome card.
    assert_flush_left_live_rows(
        &screen,
        &[
            &format!("❯ {PROMPT}"),
            MOCK_RESPONSE_SENTINEL,
            "minimal · /help",
        ],
        "after response",
    );

    // Slash menu: typing a command prefix opens the dropdown below the prompt.
    // Its rows must be flush-left too — previously the panel sat at the layout
    // hpad and the item rows one further column in (`❯ /transcript` at col 3).
    harness
        .inject_keys(b"/tra")
        .expect("type slash command prefix");
    harness
        .wait_for_text("View the conversation transcript", Duration::from_secs(10))
        .expect("slash dropdown opens");
    assert_flush_left_live_rows(
        &harness.screen_contents(),
        &["View the conversation transcript"],
        "slash dropdown",
    );
    // Close the dropdown / clear the prompt so quit isn't intercepted.
    harness.inject_keys(b"\x1b").expect("esc closes dropdown");
    harness.update(Duration::from_millis(100));

    // Permission modal: a scripted `run_terminal_command` tool call (no --yolo)
    // opens the prompt-replacing permission modal. Its rows must be flush-left
    // too — previously the whole modal sat at the layout hpad (2 columns in).
    // The accent `┃` paints the modal's first column, so a correct row has zero
    // leading spaces.
    content.set_response("PERMISSION_SETTLED — turn finished after the allow.");
    let args = json!({
        "command": "touch flush_marker.txt",
        "description": "flush-left permission check",
    })
    .to_string();
    let _permission_turn = expect_tool_turn(&content, "call_flush", "run_terminal_command", args);
    harness
        .inject_keys(b"run the flush check\r")
        .expect("submit tool prompt");
    harness
        .wait_for_text("No, reject", Duration::from_secs(30))
        .expect("permission modal opens");
    assert_flush_left_live_rows(
        &harness.screen_contents(),
        &["No, reject"],
        "permission modal",
    );
    // Allow once (shortcut `1`) so the turn settles, then wait for idle.
    harness.inject_keys(b"1").expect("allow once");
    harness
        .wait_for_text("PERMISSION_SETTLED", Duration::from_secs(30))
        .expect("turn settles after allow");
    harness
        .wait_for_text(MINIMAL_IDLE_SENTINEL, Duration::from_secs(20))
        .expect("return to idle after permission turn");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}

/// For each `needle`, find the first screen line containing it and assert that
/// line has no leading ASCII spaces (flush-left). Welcome-card rows start with
/// box-drawing chars at col 0 already; this targets the previously-padded
/// live/committed content.
fn assert_flush_left_live_rows(screen: &str, needles: &[&str], phase: &str) {
    for needle in needles {
        let line = screen
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| {
                panic!("{phase}: no screen line contains {needle:?}\nscreen:\n{screen}")
            });
        let lead = line.len() - line.trim_start_matches(' ').len();
        assert_eq!(
            lead, 0,
            "{phase}: line containing {needle:?} must be flush-left \
             (no leading spaces), got lead={lead}: {line:?}\nscreen:\n{screen}"
        );
    }
}
