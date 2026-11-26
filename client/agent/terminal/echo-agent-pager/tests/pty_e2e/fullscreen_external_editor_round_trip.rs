// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// The full TUI reaches the external editor through the command palette's
/// `/edit-prompt` entry (`Ctrl+G` stays with the tasks pane there). The
/// palette route hands the existing draft to a local non-interactive editor
/// script, restores the TUI, and leaves the edited text in the composer
/// until the user submits it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn fullscreen_external_editor_round_trip() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} edited prompt received."));

    let dir = tempfile::tempdir().expect("temp editor dir");
    // The DECRST prefix mimics a real vim exit: it resets bracketed paste,
    // mouse reporting and focus events on the shared tty, which the pager
    // must re-assert after the editor returns.
    let editor = fake_editor_command(
        dir.path(),
        "#!/bin/sh\n\
         printf '\\033[?2004l\\033[?1003l\\033[?1002l\\033[?1000l\\033[?1004l'\n\
         printf ', edited externally\\n' >> \"$1\"\n",
        "@echo off\r\n>>\"%~1\" echo|set /p=, edited externally\r\n",
    );

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env_ops(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &[EnvOp::set("VISUAL", &editor)],
    )
    .expect("spawn pager");
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered");

    inject_keys_paced(&mut harness, b"draft to polish");
    // Ctrl+P opens the command palette; picking the /edit-prompt entry
    // dispatches it draft-preservingly (unlike typing the command, which
    // would replace the composer text).
    inject_keys_paced(&mut harness, b"\x10");
    inject_keys_paced(&mut harness, b"edit-prompt");
    inject_keys_paced(&mut harness, b"\r");

    // The draft round-trips through the editor: the script appends to it.
    harness
        .wait_for_text(
            "draft to polish, edited externally",
            Duration::from_secs(10),
        )
        .expect("edited draft restored to composer");

    // The editor reset the shared terminal modes on exit (the DECRST prefix
    // in the script above); the pager must have re-asserted them, or the
    // session is left without bracketed paste and mouse reporting.
    let modes = harness.terminal_modes();
    assert!(
        modes.bracketed_paste,
        "bracketed paste must be re-enabled after the editor exits"
    );
    assert!(
        modes.mouse_reporting,
        "mouse reporting must be re-enabled after the editor exits"
    );
    assert!(
        modes.focus_in_out,
        "focus reporting must be re-enabled after the editor exits"
    );

    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} second turn done."));
    inject_keys_paced(&mut harness, b"\r");
    harness
        .wait_for_full_text("second turn done", Duration::from_secs(30))
        .expect("edited draft submitted");
    // The exact wrapped form pins both delivery and the newline strip: an
    // unstripped editor newline would arrive as `edited externally\n\n</user_query>`.
    let user_messages = all_user_message_blobs(&content);
    assert!(
        user_messages.iter().any(|message| {
            message.contains("<user_query>\ndraft to polish, edited externally\n</user_query>")
        }),
        "the exact edited draft, editor newline stripped, must reach the wire: {user_messages:#?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
