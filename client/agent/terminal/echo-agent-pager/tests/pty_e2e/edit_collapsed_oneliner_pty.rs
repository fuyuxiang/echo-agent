// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const DONE_SENTINEL: &str = "EDIT_COLLAPSED_DONE";

/// Comment planted in the replacement text: visible only when the diff body
/// renders, so it separates the collapsed one-liner from the expanded view.
const BODY_MARKER: &str = "EXPANDED_BODY_MARKER";

/// PTY: with the `collapsed_edit_blocks` flag enabled (config tier here;
/// remote settings/managed in production), an Edit tool call lands as a collapsed
/// one-liner — header with the colored `+N/-M` diffstat, no diff body — and a
/// double-click on the header expands it to the full diff. (The keyboard fold
/// keys operate on the scrollback selection, which this test does not
/// exercise.) The flag-off/legacy default is covered end-to-end by
/// `edit_hl_inplace_refresh_pty`, which relies on expanded-on-arrival diffs.
/// Doubles as the demo-cast generator via `ECHO_AGENT_PTY_CAST_DIR`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn edit_collapsed_oneliner_pty() {
    let content = ContentController::start().await.expect("start content");
    seed_ui_config(&content, "collapsed_edit_blocks = true");

    // Fixture under the isolated HOME so the scripted search_replace succeeds.
    let target = content.home().join("greet_fix.py");
    std::fs::write(
        &target,
        "# collapsed one-liner demo fixture\ndef greet():\n    return \"hi\"\n",
    )
    .expect("write fixture");
    let abs = dunce::canonicalize(&target).unwrap_or(target.clone());

    // One deleted line, two inserted lines => a `+2/-1` diffstat.
    let _tool_turn = expect_tool_turn(
        &content,
        "call_collapsed",
        "search_replace",
        json!({
            "file_path": abs.to_string_lossy(),
            "old_string": "    return \"hi\"",
            "new_string": format!("    name = \"echoagent\"  # {BODY_MARKER}\n    return f\"hi {{name}}\""),
        })
        .to_string(),
    );
    content.set_response(DONE_SENTINEL);

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--yolo", "--trust"],
        Some(content.home()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // The follow-up completion means the tool turn settled.
    harness
        .wait_for_text(DONE_SENTINEL, Duration::from_secs(90))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled transcript; got:\n{}",
                harness.screen_contents()
            )
        });

    // Collapsed one-liner: basename + colored diffstat right after it.
    harness
        .wait_for_text("Edit greet_fix.py +2/-1", Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "expected the collapsed `Edit <basename> +N/-M` one-liner; got:\n{}",
                harness.screen_contents()
            )
        });
    assert!(
        !harness.contains_text(BODY_MARKER),
        "diff body must stay hidden while collapsed\nscreen:\n{}",
        harness.screen_contents()
    );

    // Double-click the header to expand the block in place.
    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, "Edit greet_fix.py").unwrap_or_else(|| {
        panic!("could not locate the Edit header; screen:\n{screen}");
    });
    let dbl = format!(
        "{}{}{}{}",
        sgr_mouse(0, row, col, 'M'),
        sgr_mouse(0, row, col, 'm'),
        sgr_mouse(0, row, col, 'M'),
        sgr_mouse(0, row, col, 'm'),
    );
    harness
        .inject_keys(dbl.as_bytes())
        .expect("double-click header");
    harness
        .wait_for_text(BODY_MARKER, Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "double-click must expand the Edit block to the diff body; got:\n{}",
                harness.screen_contents()
            )
        });

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    #[cfg(unix)]
    write_cast_if_requested(&harness, "edit_collapsed_oneliner.cast");

    harness.quit().expect("clean quit");
}
