// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Server-driven reasoning-effort menu: a model carrying a `reasoning_efforts`
/// list renders the server labels in `/effort`, and selecting a remap row sends
/// the mapped canonical value on the wire (`deep` → `xhigh`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn reasoning_efforts_menu_renders_and_remaps_on_wire() {
    let content = ContentController::start_with_models(vec![
        MockModel::new("echo-agent-4.5")
            .with_api_backend("responses")
            .with_supports_reasoning_effort(true)
            .with_reasoning_efforts(vec![
                json!({ "id": "deep", "value": "xhigh", "label": "Deep", "description": "Maximum reasoning" }),
                json!({ "id": "balanced", "value": "medium", "label": "Balanced" }),
            ]),
    ])
    .await
    .expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} first turn."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Establish a session so the session-scoped `/effort` command is available.
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("first turn rendered");

    // (A) The server's custom label renders in the `/effort` dropdown, and the
    // built-in rows are replaced (their descriptions must be absent).
    inject_keys_paced(&mut harness, b"/effort ");
    harness
        .wait_for_text("Deep", Duration::from_secs(10))
        .expect("server label in /effort dropdown");
    assert!(
        !harness.contains_text("Faster, lighter reasoning"),
        "server list must replace the built-in rows\nscreen:\n{}",
        harness.screen_contents()
    );

    // Dismiss the dropdown and clear the composer before issuing the command.
    harness.inject_keys(keys::ESC).expect("dismiss dropdown");
    harness
        .inject_keys(b"\x15")
        .expect("clear composer (Ctrl+U)");
    harness.update(Duration::from_millis(200));

    // (B) Selecting the remap row (`deep`) sends the mapped canonical value.
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} second turn."));
    harness
        .inject_keys(b"/effort deep\r")
        .expect("set effort deep");
    harness.update(Duration::from_millis(400));
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit second prompt");
    harness
        .wait_for_text("second turn", Duration::from_secs(30))
        .expect("second turn rendered");

    let sent_xhigh = content
        .request_bodies()
        .iter()
        .any(|b| b.pointer("/reasoning/effort").and_then(|v| v.as_str()) == Some("xhigh"));
    assert!(
        sent_xhigh,
        "`/effort deep` must send the mapped canonical reasoning.effort=xhigh\nbodies: {:#?}",
        content.request_bodies()
    );

    harness.quit().expect("clean quit");
}
