// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const NESTED_TOKEN: &str = "NESTED_TOKEN";
const NESTED_OUTRO: &str = "NESTED_QUOTE_DONE";

/// PTY: drag-select copy on a NESTED quote line (`> > …`, rendered `│ │ …`)
/// excludes every nesting level's bar from the clipboard.
///
/// `SSH_CONNECTION` forces the OSC 52 clipboard route for readback, same as
/// `recap_header_not_in_selection_pty`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn nested_quote_drag_copy_excludes_bars_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!(
        "> outer quote line\n>\n> > {NESTED_TOKEN} deep quote\n\n{NESTED_OUTRO} line"
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
        .wait_for_text(NESTED_OUTRO, Duration::from_secs(45))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled response with {NESTED_OUTRO:?}; got:\n{}",
                harness.screen_contents()
            )
        });

    // Sanity: the nested row renders with both bars.
    assert!(
        harness.contains_text(&format!("│ │ {NESTED_TOKEN}")),
        "expected nested bars on screen (pretty mode)\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(10))
        .expect("scrollback focused (Space:prompt hint) after Tab");
    // Settle: the sentinel streams within the same turn, so the turn-end
    // relayout can shift rows after the waits above; locate afterwards.
    harness.update(Duration::from_millis(1500));

    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, NESTED_TOKEN).unwrap_or_else(|| {
        panic!("could not locate {NESTED_TOKEN:?}; screen:\n{screen}");
    });
    // Start on the OUTER bar ("│ │ " sits four columns left of the token) so
    // the anchor clamps past the whole excluded prefix.
    let bar_col = col.saturating_sub(4);
    let nested_line = screen.lines().nth(row as usize).unwrap_or("");
    assert_eq!(
        nested_line.chars().nth(bar_col as usize),
        Some('│'),
        "expected the outer quote bar at col {bar_col}; line: {nested_line:?}"
    );
    let end_col = col + "NESTED_TOKEN deep quote".chars().count() as u16 - 1;
    harness
        .inject_keys(mouse_drag_line(row, bar_col, end_col).as_bytes())
        .expect("drag select nested quote line");

    let payloads = wait_for_osc52_payloads(&mut harness, Duration::from_secs(10));
    assert!(
        !payloads.is_empty(),
        "expected at least one OSC 52 clipboard write; screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    assert!(
        joined.contains(&format!("{NESTED_TOKEN} deep quote")),
        "clipboard should contain the nested quote text; payloads={payloads:?}"
    );
    assert!(
        !joined.contains('│'),
        "clipboard must not contain any quote bar; payloads={payloads:?}"
    );
    assert!(
        !joined.starts_with(char::is_whitespace),
        "clipboard must not start with prefix whitespace; payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
