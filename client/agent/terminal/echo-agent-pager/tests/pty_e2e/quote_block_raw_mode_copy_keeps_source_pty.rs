// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const QUOTE_ALPHA: &str = "QUOTE_ALPHA";
const QUOTE_OUTRO: &str = "QUOTE_OUTRO_RAW_DONE";

/// PTY: per-entry raw mode (`r`) must stay byte-for-byte source-faithful —
/// drag-copying a quote line in raw mode keeps the `>` marker (the pretty-mode
/// bar exclusion must not leak into raw mode).
///
/// `SSH_CONNECTION` forces the OSC 52 clipboard route for readback, same as
/// `recap_header_not_in_selection_pty`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn quote_block_raw_mode_copy_keeps_source_pty() {
    let content = ContentController::start().await.expect("start content");
    // Bare-letter scrollback bindings (`r` = ToggleRaw) resolve only in vim
    // mode; with vim off they fall through to prompt typing (lookup_with_mode).
    seed_ui_config(&content, "vim_mode = true\nsimple_mode = false");
    content.set_response(format!(
        "intro line\n\n> {QUOTE_ALPHA} first\n\n{QUOTE_OUTRO} line"
    ));

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
        .wait_for_text(QUOTE_OUTRO, Duration::from_secs(45))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled response with {QUOTE_OUTRO:?}; got:\n{}",
                harness.screen_contents()
            )
        });

    // Pretty mode first: the bar renders, the source `>` does not.
    assert!(
        harness.contains_text("│"),
        "expected rendered quote bar before toggling raw\nscreen:\n{}",
        harness.screen_contents()
    );

    // Focus scrollback, select the agent message by clicking it (the last
    // entry is the "Worked for" event, so `k` would select that instead),
    // then toggle raw with `r` (ToggleRaw).
    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(10))
        .expect("scrollback focused (Space:prompt hint) after Tab");
    harness.update(Duration::from_millis(1000));
    let screen = harness.screen_contents();
    let (qrow, qcol) = locate_screen_text(&screen, QUOTE_ALPHA).unwrap_or_else(|| {
        panic!("could not locate {QUOTE_ALPHA:?} before raw toggle; screen:\n{screen}");
    });
    let click = format!(
        "{}{}",
        sgr_mouse(0, qrow, qcol, 'M'),
        sgr_mouse(0, qrow, qcol, 'm')
    );
    harness
        .inject_keys(click.as_bytes())
        .expect("click to select the agent message entry");
    harness.update(Duration::from_millis(300));
    harness.inject_keys(b"r").expect("toggle raw mode");

    let raw_quote = format!("> {QUOTE_ALPHA} first");
    harness
        .wait_for_text(&raw_quote, Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "expected raw source {raw_quote:?} after `r`; screen:\n{}",
                harness.screen_contents()
            )
        });
    // Settle the raw-toggle relayout before locating drag coordinates.
    harness.update(Duration::from_millis(500));

    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, &raw_quote).unwrap_or_else(|| {
        panic!("could not locate {raw_quote:?}; screen:\n{screen}");
    });
    let end_col = col + raw_quote.chars().count() as u16 - 1;
    harness
        .inject_keys(mouse_drag_line(row, col, end_col).as_bytes())
        .expect("drag select raw quote line");

    let payloads = wait_for_osc52_payloads(&mut harness, Duration::from_secs(10));
    assert!(
        !payloads.is_empty(),
        "expected at least one OSC 52 clipboard write; screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    assert!(
        joined.contains(&raw_quote),
        "raw-mode clipboard must keep the source `>` marker; payloads={payloads:?}"
    );
    assert!(
        !joined.contains('│'),
        "raw mode never renders bars, so none may be copied; payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
