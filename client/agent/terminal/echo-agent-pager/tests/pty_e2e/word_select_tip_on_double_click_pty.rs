// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Unique prefix of the word-select settings tip:
/// `Want double-click to select? /settings → Text selection · Ctrl+Y: enable now`.
pub(crate) const WORD_SELECT_TIP_SENTINEL: &str = "Want double-click to select";

/// Stable body text we double-click in scrollback (also the mock response).
const BODY_SENTINEL: &str = "WORDSELECTBODY";

fn double_click_at(harness: &mut PtyHarness, row: u16, col: u16) {
    let dbl = format!(
        "{}{}{}{}",
        sgr_mouse(0, row, col, 'M'),
        sgr_mouse(0, row, col, 'm'),
        sgr_mouse(0, row, col, 'M'),
        sgr_mouse(0, row, col, 'm'),
    );
    harness
        .inject_keys(dbl.as_bytes())
        .expect("inject SGR double-click");
}

fn spawn_with_hints(content: &ContentController) -> PtyHarness {
    let binary = pager_binary().expect("resolve pager binary");
    let env_refs = CONTEXTUAL_HINTS_ENV;
    PtyHarness::spawn_with_content_env(&binary, DEFAULT_ROWS, DEFAULT_COLS, content, &[], env_refs)
        .expect("spawn pager with contextual hints")
}

/// Double-click scrollback while Text selection is fold/nav (`flash`) →
/// ephemeral tip advertises the settings path and the Ctrl+Y accept chord;
/// pressing Ctrl+Y while the tip is up flips the setting to `word_select`
/// and persists it to config.toml.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
async fn word_select_tip_shows_and_ctrl_y_accepts() {
    let content = ContentController::start().await.expect("start content");
    // Default is flash (fold/nav). Pin it so a sibling test's seed can't leak.
    seed_ui_config(&content, "keep_text_selection = \"flash\"");
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} {BODY_SENTINEL} unique payload for tip e2e."
    ));

    let mut harness = spawn_with_hints(&content);
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(BODY_SENTINEL, Duration::from_secs(30))
        .expect("assistant body rendered");
    harness.update(Duration::from_millis(400));

    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Ctrl+e:", Duration::from_secs(10))
        .expect("scrollback owns keys");

    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, BODY_SENTINEL).unwrap_or_else(|| {
        panic!("locate body for double-click; screen:\n{screen}");
    });
    // Click mid-word so the hit lands on selectable text columns. The FIRST
    // double-click gesture is treated as intentional folding — no tip.
    double_click_at(&mut harness, row, col + 2);
    harness.update(Duration::from_millis(600));
    assert!(
        !harness.contains_text(WORD_SELECT_TIP_SENTINEL),
        "a lone double-click must not tip; screen:\n{}",
        harness.screen_contents()
    );

    // The REPEATED gesture (separate multi-click window, inside the repeat
    // window) is the selection-attempt signal that fires the tip.
    double_click_at(&mut harness, row, col + 2);

    harness
        .wait_for_text(WORD_SELECT_TIP_SENTINEL, Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "word-select tip must show after repeated fold/nav double-click: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });
    assert!(
        harness.contains_text("/settings") || harness.contains_text("Text selection"),
        "tip should advertise settings path; screen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.contains_text("Ctrl+Y"),
        "tip should advertise the accept chord; screen:\n{}",
        harness.screen_contents()
    );

    // Accept via the advertised chord while the tip is on screen.
    harness.inject_keys(b"\x19").expect("Ctrl+Y accept");
    harness
        .wait_for_text("Text selection: word_select", Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "accept toast must confirm the flip: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    // The flip persists: `[ui].keep_text_selection = "word_select"` lands in
    // config.toml (async persist — poll briefly).
    let config_path = content.home().join(".echo-agent").join("config.toml");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let config = std::fs::read_to_string(&config_path).unwrap_or_default();
        if config.contains("keep_text_selection = \"word_select\"") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "keep_text_selection = word_select never persisted; config:\n{config}"
        );
        harness.update(Duration::from_millis(200));
    }

    // With word_select live, another double-click selects instead of tipping.
    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, BODY_SENTINEL).unwrap_or_else(|| {
        panic!("locate body for second double-click; screen:\n{screen}");
    });
    double_click_at(&mut harness, row, col + 2);
    harness.update(Duration::from_millis(800));
    assert!(
        !harness.contains_text(WORD_SELECT_TIP_SENTINEL),
        "tip must not reappear once word_select is on; screen:\n{}",
        harness.screen_contents()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}

/// Already on `word_select` → double-click selects a word; tip must not fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
async fn word_select_tip_skipped_when_mode_is_word_select() {
    let content = ContentController::start().await.expect("start content");
    seed_ui_config(&content, "keep_text_selection = \"word_select\"");
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} {BODY_SENTINEL} unique payload for tip e2e."
    ));

    let mut harness = spawn_with_hints(&content);
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(BODY_SENTINEL, Duration::from_secs(30))
        .expect("assistant body rendered");
    harness.update(Duration::from_millis(400));

    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Ctrl+e:", Duration::from_secs(10))
        .expect("scrollback owns keys");

    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, BODY_SENTINEL).unwrap_or_else(|| {
        panic!("locate body for double-click; screen:\n{screen}");
    });
    // Two separate gestures — the repeat gate would pass, so absence here
    // proves the word_select-mode gate, not the repeat gate.
    double_click_at(&mut harness, row, col + 2);
    harness.update(Duration::from_millis(600));
    double_click_at(&mut harness, row, col + 2);

    // Give the multi-click path time to run; tip must not appear.
    harness.update(Duration::from_millis(800));
    assert!(
        !harness.contains_text(WORD_SELECT_TIP_SENTINEL),
        "tip must not show when already on word_select; screen:\n{}",
        harness.screen_contents()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}

/// Per-tip gate off (`[ui.contextual_hints].word_select = false`) → no tip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
async fn word_select_tip_skipped_when_contextual_hint_disabled() {
    let content = ContentController::start().await.expect("start content");
    // flash mode + tip explicitly disabled. ECHO_AGENT_CONTEXTUAL_HINTS is NOT set —
    // that env master would force all tips on and defeat the config opt-out.
    let echo_agent_home = content.home().join(".echo-agent");
    std::fs::create_dir_all(&echo_agent_home).expect("create .echo-agent");
    std::fs::write(
        echo_agent_home.join("config.toml"),
        "[ui]\n\
         keep_text_selection = \"flash\"\n\
         [ui.contextual_hints]\n\
         word_select = false\n",
    )
    .expect("write config.toml");
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} {BODY_SENTINEL} unique payload for tip e2e."
    ));

    let binary = pager_binary().expect("resolve pager binary");
    // Content env only — pin the env master to empty (parsed as unset) so an
    // inherited ECHO_AGENT_CONTEXTUAL_HINTS from the runner's shell can't force
    // tips on and defeat the config opt-out under test.
    let overrides: Vec<(String, String)> =
        vec![("ECHO_AGENT_CONTEXTUAL_HINTS".into(), String::new())];
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &env_refs,
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(BODY_SENTINEL, Duration::from_secs(30))
        .expect("assistant body rendered");
    harness.update(Duration::from_millis(400));

    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Ctrl+e:", Duration::from_secs(10))
        .expect("scrollback owns keys");

    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, BODY_SENTINEL).unwrap_or_else(|| {
        panic!("locate body for double-click; screen:\n{screen}");
    });
    // Two separate gestures — the repeat gate would pass, so absence here
    // proves the per-tip config opt-out, not the repeat gate.
    double_click_at(&mut harness, row, col + 2);
    harness.update(Duration::from_millis(600));
    double_click_at(&mut harness, row, col + 2);
    harness.update(Duration::from_millis(800));

    assert!(
        !harness.contains_text(WORD_SELECT_TIP_SENTINEL),
        "tip must not show when contextual_hints.word_select is false; screen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
