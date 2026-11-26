// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const DONE_SENTINEL: &str = "VERB_GROUP_DRAG_DONE";

/// Wait for an OSC 52 payload BEYOND the first `seen` ones and return the new
/// tail. `decode_osc52_payloads` reads the whole raw stream, so each drag's
/// assertions must scope to its own delta.
fn wait_for_new_osc52(harness: &mut PtyHarness, seen: usize, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        harness.update(Duration::from_millis(200));
        let payloads = decode_osc52_payloads(harness.raw_output());
        if payloads.len() > seen || Instant::now() >= deadline {
            return payloads.into_iter().skip(seen).collect();
        }
    }
}

/// Drag across `needle` on the current screen and return the drag's new
/// OSC 52 payloads, joined. A settle pause first so the mouse press can't
/// chain into the previous gesture's multi-click window.
fn drag_copy(harness: &mut PtyHarness, needle: &str) -> String {
    harness.update(Duration::from_millis(600));
    let seen = decode_osc52_payloads(harness.raw_output()).len();
    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, needle)
        .unwrap_or_else(|| panic!("could not locate {needle:?}; screen:\n{screen}"));
    let end_col = col + needle.chars().count() as u16 - 1;
    harness
        .inject_keys(mouse_drag_line(row, col, end_col).as_bytes())
        .expect("drag select");
    let payloads = wait_for_new_osc52(harness, seen, Duration::from_secs(10));
    assert!(
        !payloads.is_empty(),
        "drag over {needle:?} must emit an OSC 52 write; screen:\n{}",
        harness.screen_contents()
    );
    payloads.join("\n")
}

/// PTY: drag-copy on a verb-group header yields the aggregated label — folded
/// AND expanded — and on the expanded slot the header row and member 0's row
/// are independent targets: dragging the member's path never drags the header
/// label along (the two rows previously shared one selectable range).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn verb_group_header_drag_copy_pty() {
    let content = ContentController::start().await.expect("start content");
    // Pin ON via the config tier so the test doesn't ride the client default.
    seed_ui_config(&content, "group_tool_verbs = true");

    // Seed real files under the isolated HOME so the reads succeed.
    let mut paths = Vec::new();
    for name in ["dragme1.txt", "dragme2.txt"] {
        let path = content.home().join(name);
        std::fs::write(&path, "hello drag copy\n").expect("write fixture file");
        paths.push(dunce::canonicalize(&path).unwrap_or(path));
    }
    let _tool_turns: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            expect_tool_turn(
                &content,
                &format!("call_d{i}"),
                "read_file",
                json!({ "target_file": p.to_string_lossy() }).to_string(),
            )
        })
        .collect();
    content.set_response(DONE_SENTINEL);

    let binary = pager_binary().expect("resolve pager binary");
    // SSH_CONNECTION so macOS routes the copy through OSC 52 (readback path);
    // same pattern as read_tool_header_selection_copies_path_only_pty.
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
        .wait_for_text(DONE_SENTINEL, Duration::from_secs(90))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled transcript; got:\n{}",
                harness.screen_contents()
            )
        });
    harness
        .wait_for_text("Read 2 files", Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "expected folded verb-group header; got:\n{}",
                harness.screen_contents()
            )
        });

    // Focus scrollback so the drags target the transcript, not the prompt.
    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(10))
        .expect("scrollback focused (Space:prompt hint) after Tab");

    // FOLDED header: the drag copies the aggregated label, not the hidden
    // member block's own text.
    let folded_copy = drag_copy(&mut harness, "Read 2 files");
    assert!(
        folded_copy.contains("Read 2 files") && !folded_copy.contains("dragme1"),
        "folded header drag must copy the label; payloads={folded_copy:?}"
    );

    // Expand the group: double-click on the header row.
    harness.update(Duration::from_millis(600));
    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, "Read 2 files").unwrap_or_else(|| {
        panic!("could not locate verb-group header; screen:\n{screen}");
    });
    let click = format!(
        "{}{}{}{}",
        sgr_mouse(0, row, col, 'M'),
        sgr_mouse(0, row, col, 'm'),
        sgr_mouse(0, row, col, 'M'),
        sgr_mouse(0, row, col, 'm'),
    );
    harness
        .inject_keys(click.as_bytes())
        .expect("double-click header");
    harness
        .wait_for_text("dragme1.txt", Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "expected member rows while expanded; got:\n{}",
                harness.screen_contents()
            )
        });

    // EXPANDED member 0 row: the drag copies the member's own text only —
    // the header label one row above must not ride along.
    let member_copy = drag_copy(&mut harness, "dragme1.txt");
    assert!(
        member_copy.contains("dragme1.txt") && !member_copy.contains("Read 2 files"),
        "member drag must not drag the header along; payloads={member_copy:?}"
    );

    // EXPANDED header row: the drag still copies the label only.
    let expanded_copy = drag_copy(&mut harness, "Read 2 files");
    assert!(
        expanded_copy.contains("Read 2 files") && !expanded_copy.contains("dragme1"),
        "expanded header drag must copy the label only; payloads={expanded_copy:?}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
