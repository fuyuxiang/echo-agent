// Per-test-case module for the `pty_e2e` integration test crate.
//
// Plugins-tab footer shows contextual Space enable/disable (not "toggle")
// and freeform `a install` (not `a add`). Run with `--nocapture` to dump
// screen contents when debugging failures.
#[allow(unused_imports)]
use super::common::*;

const ENABLED_PLUGIN: &str = "copy-enabled";
const DISABLED_PLUGIN: &str = "copy-disabled";

fn seed_plugins_for_copy_hints(content: &ContentController) {
    let echo_agent_home = content.home().join(".echo-agent");
    let plugins_dir = echo_agent_home.join("plugins");
    for name in [ENABLED_PLUGIN, DISABLED_PLUGIN] {
        let dir = plugins_dir.join(name);
        std::fs::create_dir_all(&dir).expect("create plugin dir");
        std::fs::write(
            dir.join("plugin.json"),
            format!(
                r#"{{"name":"{name}","version":"0.0.1","description":"extensions modal copy fixture"}}"#
            ),
        )
        .expect("write plugin.json");
    }
    std::fs::create_dir_all(&echo_agent_home).expect("create .echo-agent");
    // User plugins default to disabled unless listed under enabled.
    let config = format!(
        "[plugins]\nenabled = [\"{ENABLED_PLUGIN}\"]\ndisabled = [\"{DISABLED_PLUGIN}\"]\n"
    );
    std::fs::write(echo_agent_home.join("config.toml"), config).expect("write config.toml");
}

fn dump_screen(label: &str, harness: &PtyHarness) {
    let screen = harness.screen_contents();
    eprintln!(
        "\n========== PTY CAPTURE: {label} ==========\n{screen}\n========== END: {label} ==========\n"
    );
}

/// True when the screen shows the Space footer verb we asked for.
///
/// Important: `space enable` is a substring of the fallback
/// `space enable/disable`, so a naive `contains("space enable")` would
/// pass while the contextual hint is still the combined form.
fn screen_has_space_verb(screen: &str, verb: &str) -> bool {
    match verb {
        "enable/disable" => screen.contains("space enable/disable"),
        "enable" => screen.contains("space enable") && !screen.contains("space enable/disable"),
        "disable" => screen.contains("space disable"),
        _ => screen.contains(&format!("space {verb}")),
    }
}

fn wait_for_space_verb(harness: &mut PtyHarness, verb: &str) {
    let needle = format!("space {verb}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if screen_has_space_verb(&harness.screen_contents(), verb) {
            return;
        }
        harness.update(Duration::from_millis(150));
    }
    dump_screen(&format!("missing {needle}"), harness);
    panic!("expected footer hint `{needle}` (not a fallback false-positive)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn extensions_modal_copy_hints_pty() {
    let content = ContentController::start().await.expect("start content");
    seed_plugins_for_copy_hints(&content);

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    harness.inject_keys(b"/plugins\r").expect("submit /plugins");
    harness
        .wait_for_text("Plugins", Duration::from_secs(15))
        .expect("extensions modal Plugins tab chrome");

    // Both fixture plugins share one source group, seeded collapsed on load.
    // Expand it (selection starts on the header row) so the rows are visible.
    harness
        .wait_for_text("(2 plugins)", Duration::from_secs(20))
        .expect("plugin source group header");
    harness.inject_keys(b"l").expect("expand plugin group");
    harness
        .wait_for_text(ENABLED_PLUGIN, Duration::from_secs(20))
        .expect("enabled plugin row");
    harness
        .wait_for_text(DISABLED_PLUGIN, Duration::from_secs(10))
        .expect("disabled plugin row");

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if harness.contains_text("a install") {
            break;
        }
        harness.update(Duration::from_millis(150));
    }
    assert!(
        harness.contains_text("a install"),
        "Plugins tab must show `a install` (not `a add`)\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("a add"),
        "Plugins tab must not show legacy `a add`\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("space toggle"),
        "Plugins tab must not show legacy `space toggle`\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(b"/").expect("start search");
    harness.update(Duration::from_millis(200));
    for ch in DISABLED_PLUGIN.bytes() {
        harness
            .inject_keys(std::slice::from_ref(&ch))
            .expect("type filter char");
        harness.update(Duration::from_millis(30));
    }
    harness.update(Duration::from_millis(300));
    harness.inject_keys(b"\r").expect("commit search");
    harness.update(Duration::from_millis(400));
    // Search results keep the group header as row 0; step onto the plugin row.
    harness.inject_keys(b"j").expect("move to plugin row");
    harness.update(Duration::from_millis(200));
    wait_for_space_verb(&mut harness, "enable");
    dump_screen("plugins-tab disabled selected → space enable", &harness);
    assert!(
        harness.contains_text("a install"),
        "install hint must remain with space enable\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(b"/").expect("start search again");
    harness.update(Duration::from_millis(200));
    harness.inject_keys(b"\x15").expect("Ctrl+U clear query");
    harness.update(Duration::from_millis(100));
    for ch in ENABLED_PLUGIN.bytes() {
        harness
            .inject_keys(std::slice::from_ref(&ch))
            .expect("type filter char");
        harness.update(Duration::from_millis(30));
    }
    harness.update(Duration::from_millis(300));
    harness.inject_keys(b"\r").expect("commit search enabled");
    harness.update(Duration::from_millis(400));
    harness.inject_keys(b"j").expect("move to plugin row");
    harness.update(Duration::from_millis(200));
    wait_for_space_verb(&mut harness, "disable");
    dump_screen("plugins-tab enabled selected → space disable", &harness);
    assert!(
        harness.contains_text("a install"),
        "install hint must remain with space disable\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(b"/").expect("start search empty");
    harness.update(Duration::from_millis(200));
    harness.inject_keys(b"\x15").expect("Ctrl+U clear");
    for ch in b"zzz-no-such-plugin" {
        harness
            .inject_keys(std::slice::from_ref(ch))
            .expect("type no-match filter");
        harness.update(Duration::from_millis(30));
    }
    harness.update(Duration::from_millis(300));
    harness.inject_keys(b"\r").expect("commit empty search");
    harness.update(Duration::from_millis(400));
    wait_for_space_verb(&mut harness, "enable/disable");
    dump_screen(
        "plugins-tab no matching selection → space enable/disable",
        &harness,
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
