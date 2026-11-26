// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Unique body token so we can locate the recap summary without matching the
/// "Recap" chrome label (or unrelated scrollback text).
const RECAP_BODY_TOKEN: &str = "RECAP_BODY_SEL_TOKEN";

/// PTY: drag-select on an expanded recap copies only the summary body, never
/// the "Recap" header label. Mirrors Thinking-header / tool-label exclusion.
///
/// Flow: one agent turn (so recap_gate has a main turn) → `/recap` → second
/// mock inference response becomes the recap body → drag the body → OSC 52.
///
/// `SSH_CONNECTION` is set deliberately: on macOS the clipboard route only
/// emits OSC 52 when it believes the session is remote (see
/// `resolve_clipboard_route`); the harness strips inherited SSH vars, so
/// scripted scenarios and this test re-inject a dummy one for OSC 52
/// readback — same pattern as `tests/scenarios/copy_selection.yaml`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn recap_header_not_in_selection_pty() {
    let content = ContentController::start().await.expect("start content");

    // First agent turn — any non-empty reply so recap_gate has a main turn.
    // Recap is a separate inference call; we swap the fixed mock body after
    // this turn settles so only the recap carries RECAP_BODY_TOKEN.
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} first turn for recap context."
    ));

    let binary = pager_binary().expect("resolve pager binary");
    // Force OSC 52 so we can assert clipboard contents via the PTY raw stream
    // (macOS otherwise uses the native pasteboard only).
    let overrides: Vec<(String, String)> = vec![(
        "SSH_CONNECTION".into(),
        "scripted-test 1 127.0.0.1 2".into(),
    )];
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut harness = PtyHarness::spawn_with_content_env_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &env_refs,
        Some(content.home()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");

    // Establish a session with at least one main turn (recap_gate requires it).
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(45))
        .expect("first turn response");

    // Recap reuses chat completions / responses with a synthetic instruction
    // turn; point the mock at a unique body so we can locate it unambiguously.
    content.set_response(format!(
        "We fixed text selection so only {RECAP_BODY_TOKEN} is copyable."
    ));

    // Manual /recap — session_recap is ON by default in the shell.
    harness.inject_keys(b"/recap\r").expect("submit /recap");
    harness
        .wait_for_text(RECAP_BODY_TOKEN, Duration::from_secs(45))
        .unwrap_or_else(|_| {
            panic!(
                "expected recap body token on screen; got:\n{}",
                harness.screen_contents()
            )
        });

    // Focus scrollback so the drag targets the recap body, not the prompt.
    harness.inject_keys(b"\t").expect("focus scrollback");
    harness.update(Duration::from_millis(400));

    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, RECAP_BODY_TOKEN).unwrap_or_else(|| {
        panic!("could not locate {RECAP_BODY_TOKEN:?} after wait; screen:\n{screen}");
    });
    let end_col = col + RECAP_BODY_TOKEN.chars().count() as u16 - 1;

    // Drag across the body token. Header is Selectable::None / no selection_range,
    // so even a multi-line select that would have included "Recap" only copies body.
    harness
        .inject_keys(mouse_drag_line(row, col, end_col).as_bytes())
        .expect("drag select recap body");

    let payloads = wait_for_osc52_payloads(&mut harness, Duration::from_secs(10));

    assert!(
        !payloads.is_empty(),
        "expected at least one OSC 52 clipboard write; screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    assert!(
        joined.contains(RECAP_BODY_TOKEN),
        "clipboard should contain the recap body token; payloads={payloads:?}"
    );
    // Full-block selection would include the chrome label as a line.
    assert!(
        !joined.lines().any(|l| l.trim() == "Recap"),
        "clipboard must not include a standalone 'Recap' header line; payloads={payloads:?}"
    );
    assert!(
        !joined.contains(&format!("Recap\n{RECAP_BODY_TOKEN}"))
            && !joined.contains(&format!("Recap {RECAP_BODY_TOKEN}")),
        "clipboard must not prefix the body with the Recap label; payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
