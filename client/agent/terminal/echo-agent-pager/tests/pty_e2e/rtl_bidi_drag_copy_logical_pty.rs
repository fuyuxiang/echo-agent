// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// ASCII sentinel that anchors the mixed RTL row and settles the turn.
const RTL_SENTINEL: &str = "RTLSENT";
const RTL_OUTRO: &str = "RTL_OUTRO_DONE";
/// Persian "khoob" (good). Chosen with no lam-alef ligature so every letter
/// occupies exactly one display column (1:1 logical↔visual column mapping).
const FA_LOGICAL: &str = "خوب";
/// Its visual (reversed) form — what an app-reordered row paints.
const FA_VISUAL: &str = "بوخ";

/// PTY: with `[scrollback.display] rtl_bidi = true`, a mixed LTR+Persian row
/// paints the Persian in visual (reversed) order, but a drag-copy over those
/// painted cells puts the *logical* order on the clipboard. This is the
/// end-to-end guard for the selection↔paint contract: cells are visual, the
/// clipboard is logical.
///
/// `SSH_CONNECTION` is set so the macOS clipboard route emits OSC 52 for
/// readback — same pattern as `quote_block_drag_copy_excludes_bars_pty`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn rtl_bidi_drag_copy_logical_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!(
        "intro line\n\n{RTL_SENTINEL} {FA_LOGICAL}\n\n{RTL_OUTRO} line"
    ));

    // Enable app-side RTL reordering via appearance config (pager.toml).
    let echo_agent_home = content.home().join(".echo-agent");
    std::fs::create_dir_all(&echo_agent_home).expect("create .echo-agent");
    std::fs::write(
        echo_agent_home.join("pager.toml"),
        "[scrollback.display]\nrtl_bidi = true\n",
    )
    .expect("write pager.toml");

    let binary = pager_binary().expect("resolve pager binary");
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
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(RTL_OUTRO, Duration::from_secs(45))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled response with {RTL_OUTRO:?}; got:\n{}",
                harness.screen_contents()
            )
        });

    // The row must paint in VISUAL order ("بوخ"), proving reorder is on.
    assert!(
        harness.contains_text(FA_VISUAL),
        "expected app-reordered (visual) Persian {FA_VISUAL:?} on screen\nscreen:\n{}",
        harness.screen_contents()
    );

    // Focus scrollback so the drag targets the message, not the prompt.
    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(10))
        .expect("scrollback focused after Tab");
    harness.update(Duration::from_millis(1500));

    let screen = harness.screen_contents();
    let (row, sentinel_col) = locate_screen_text(&screen, RTL_SENTINEL).unwrap_or_else(|| {
        panic!("could not locate {RTL_SENTINEL:?}; screen:\n{screen}");
    });
    // Row layout (LTR base): "RTLSENT" then a space, then the reversed Persian.
    // Persian occupies the three cells after "RTLSENT ".
    let fa_start = sentinel_col + RTL_SENTINEL.chars().count() as u16 + 1;
    let fa_end = fa_start + FA_LOGICAL.chars().count() as u16 - 1;
    // Sanity: the leftmost painted Persian cell is the last logical letter.
    let line = screen.lines().nth(row as usize).unwrap_or("");
    assert!(
        line.contains(FA_VISUAL),
        "row should paint visual Persian; line: {line:?}"
    );

    // Drag across the whole mixed row (sentinel through the Persian) and copy.
    let mut drag = String::new();
    drag.push_str(&sgr_mouse(0, row, sentinel_col, 'M'));
    drag.push_str(&sgr_mouse(32, row, fa_start, 'M'));
    drag.push_str(&sgr_mouse(32, row, fa_end, 'M'));
    harness.inject_keys(drag.as_bytes()).expect("drag over row");
    harness.update(Duration::from_millis(300));
    harness
        .inject_keys(sgr_mouse(0, row, fa_end, 'm').as_bytes())
        .expect("release drag");

    let payloads = wait_for_osc52_payloads(&mut harness, Duration::from_secs(10));
    assert!(
        !payloads.is_empty(),
        "expected an OSC 52 clipboard write; screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    // Clipboard is LOGICAL order ("خوب"), never the painted visual ("بوخ").
    assert!(
        joined.contains(FA_LOGICAL),
        "clipboard should contain logical Persian {FA_LOGICAL:?}; payloads={payloads:?}"
    );
    assert!(
        !joined.contains(FA_VISUAL),
        "clipboard must NOT contain visual (reversed) Persian {FA_VISUAL:?}; payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
