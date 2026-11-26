// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const DONE_SENTINEL: &str = "VERB_GROUP_TOGGLE_DONE";

/// F2 → filter `group tool calls` (unique Bool row) → Enter commit → Space
/// toggle once → assert the row value. Mirrors the show-thinking-blocks
/// toggle e2e helper.
fn toggle_group_tool_calls(harness: &mut PtyHarness, want_on: bool) {
    const F2: &[u8] = b"\x1bOQ";
    if harness.contains_text("Space:prompt") {
        harness.inject_keys(b" ").expect("Space:prompt");
        harness.update(Duration::from_millis(200));
    }

    harness.inject_keys(F2).expect("F2 open settings");
    harness.update(Duration::from_millis(500));

    harness.inject_keys(b"/").expect("start filter");
    harness.update(Duration::from_millis(150));
    for ch in b"group tool calls" {
        harness
            .inject_keys(std::slice::from_ref(ch))
            .expect("filter char");
        harness.update(Duration::from_millis(30));
    }
    harness.update(Duration::from_millis(300));
    assert!(
        harness.contains_text("Group tool calls"),
        "settings filter must show Group tool calls row\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(b"\r").expect("commit filter");
    harness.update(Duration::from_millis(300));
    harness.inject_keys(b" ").expect("toggle group_tool_verbs");
    harness.update(Duration::from_millis(500));

    let want_label = if want_on { "on" } else { "off" };
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut saw_value = false;
    while Instant::now() < deadline {
        let screen = harness.screen_contents();
        for line in screen.lines() {
            if line.contains("Group tool calls") && line.split_whitespace().any(|w| w == want_label)
            {
                saw_value = true;
                break;
            }
        }
        if saw_value {
            break;
        }
        harness.update(Duration::from_millis(150));
    }
    assert!(
        saw_value,
        "settings row must show Group tool calls = {want_label}\nscreen:\n{}",
        harness.screen_contents()
    );

    for _ in 0..4 {
        if !harness.contains_text("Group tool calls") && !harness.contains_text("Appearance") {
            break;
        }
        harness.inject_keys(keys::ESC).expect("esc settings");
        harness.update(Duration::from_millis(250));
    }
}

/// PTY: the settings-modal "Group tool calls" toggle re-lays-out the LIVE
/// transcript. Turning it OFF unfolds an existing "Read 3 files" group into
/// individual Read rows immediately (no `/new` needed — the toggle path must
/// invalidate cached entry heights); turning it back ON refolds them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn verb_group_settings_toggle_pty() {
    let content = ContentController::start().await.expect("start content");
    // Pin ON via the CONFIG tier, not the env var — env outranks config in
    // the resolve chain, so an env pin could not be overridden by the
    // modal's config write if anything re-resolves the full chain mid-test
    // (e.g. a settings update); the config seed keeps the modal toggle in
    // control of the effective value on every path.
    seed_ui_config(&content, "group_tool_verbs = true");

    // Seed real files under the isolated HOME so the reads succeed.
    let mut paths = Vec::new();
    for name in ["t1.txt", "t2.txt", "t3.txt"] {
        let path = content.home().join(name);
        std::fs::write(&path, "hello verb group\n").expect("write fixture file");
        paths.push(dunce::canonicalize(&path).unwrap_or(path));
    }
    let _tool_turns: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            expect_tool_turn(
                &content,
                &format!("call_t{i}"),
                "read_file",
                json!({ "target_file": p.to_string_lossy() }).to_string(),
            )
        })
        .collect();
    content.set_response(DONE_SENTINEL);

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(DONE_SENTINEL, Duration::from_secs(120))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled transcript; got:\n{}",
                harness.screen_contents()
            )
        });
    harness
        .wait_for_text("Read 3 files", Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "expected folded header before toggle; got:\n{}",
                harness.screen_contents()
            )
        });

    toggle_group_tool_calls(&mut harness, false);
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut unfolded = false;
    while Instant::now() < deadline {
        harness.update(Duration::from_millis(150));
        let s = harness.screen_contents();
        if !s.contains("Read 3 files")
            && s.contains("t1.txt")
            && s.contains("t2.txt")
            && s.contains("t3.txt")
        {
            unfolded = true;
            break;
        }
    }
    assert!(
        unfolded,
        "toggle OFF must unfold the existing transcript immediately\nscreen:\n{}",
        harness.screen_contents()
    );

    toggle_group_tool_calls(&mut harness, true);
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut refolded = false;
    while Instant::now() < deadline {
        harness.update(Duration::from_millis(150));
        let s = harness.screen_contents();
        if s.contains("Read 3 files") && !s.contains("t2.txt") {
            refolded = true;
            break;
        }
    }
    assert!(
        refolded,
        "toggle ON must refold the transcript\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
