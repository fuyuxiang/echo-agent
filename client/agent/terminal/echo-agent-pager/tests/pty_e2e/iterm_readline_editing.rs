// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const CTRL_BACKSLASH: &[u8] = b"\x1b[92;5u";
const OPTION_BACKSPACE: &[u8] = b"\x1b\x7f";
const META_B: &[u8] = b"\x1bb";
const META_F: &[u8] = b"\x1bf";
const ALT_LEFT: &[u8] = b"\x1b[1;3D";
const ALT_RIGHT: &[u8] = b"\x1b[1;3C";
const ROW_TITLE: &str = "ITERMROW";

fn click_visible_text(harness: &mut PtyHarness, text: &str) {
    harness
        .wait_for_text(text, Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("{text:?} did not render\n{}", harness.screen_contents()));
    let screen = harness.screen_contents();
    let (row, col) = screen
        .lines()
        .enumerate()
        .find_map(|(row, line)| {
            let byte = line.find(text)?;
            let prefix_width = unicode_width::UnicodeWidthStr::width(&line[..byte]) as u16;
            let text_width = unicode_width::UnicodeWidthStr::width(text) as u16;
            Some((row as u16, prefix_width + text_width / 2))
        })
        .unwrap_or_else(|| panic!("could not locate {text:?}\n{screen}"));
    let click = format!(
        "{}{}",
        sgr_mouse(0, row, col, 'M'),
        sgr_mouse(0, row, col, 'm')
    );
    harness
        .inject_keys(click.as_bytes())
        .unwrap_or_else(|error| panic!("click {text:?}: {error}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; CI runs the ignored pty_e2e suite"]
async fn iterm_raw_readline_sequences_edit_picker_and_dashboard_rename() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} iTerm editing turn."));
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env_ops(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &[EnvOp::set("TERM_PROGRAM", "iTerm.app")],
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit setup prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered");

    inject_keys_paced(&mut harness, format!("/rename {ROW_TITLE}").as_bytes());
    harness
        .inject_keys(keys::ENTER)
        .expect("seed session title");
    harness
        .wait_for_text("Session renamed to", Duration::from_secs(15))
        .expect("seeded title persisted");

    harness.inject_keys(b"\x10").expect("Ctrl+P");
    harness
        .wait_for_text("Commands", Duration::from_secs(10))
        .expect("command palette open");
    inject_keys_paced(&mut harness, b"ITERMONE ITERMDELETE");
    harness
        .wait_for_text("ITERMONE ITERMDELETE", Duration::from_secs(10))
        .expect("palette query rendered");
    harness
        .inject_keys(OPTION_BACKSPACE)
        .expect("iTerm Option+Backspace");
    wait_for_labels_absent(&mut harness, &["ITERMDELETE"], Duration::from_secs(10));
    inject_keys_paced(&mut harness, b"ITERMWORD");
    harness.inject_keys(META_B).expect("iTerm Meta-B");
    inject_keys_paced(&mut harness, b"MID");
    harness.inject_keys(META_F).expect("iTerm Meta-F");
    inject_keys_paced(&mut harness, b"END");
    harness
        .wait_for_text("ITERMONE MIDITERMWORDEND", Duration::from_secs(10))
        .expect("raw Meta editing changed palette query");

    harness.inject_keys(keys::ESC).expect("clear palette query");
    wait_for_labels_absent(
        &mut harness,
        &["ITERMONE MIDITERMWORDEND"],
        Duration::from_secs(10),
    );
    harness
        .inject_keys(keys::ESC)
        .expect("close command palette");
    wait_for_labels_absent(&mut harness, &["Commands"], Duration::from_secs(10));

    harness.inject_keys(CTRL_BACKSLASH).expect("open dashboard");
    harness
        .wait_for_text("+ New Agent", Duration::from_secs(10))
        .expect("dashboard open");
    click_visible_text(&mut harness, ROW_TITLE);
    harness
        .wait_for_text("[Dashboard]", Duration::from_secs(10))
        .expect("row click attached the dashboard overlay");
    harness
        .inject_keys(keys::ESC)
        .expect("close attached dashboard row");
    wait_for_labels_absent(&mut harness, &["[Dashboard]"], Duration::from_secs(10));
    harness
        .inject_keys(b"\x12")
        .expect("dashboard Ctrl+R rename");
    harness
        .wait_for_text(&format!("rename: {ROW_TITLE}"), Duration::from_secs(10))
        .expect("rename editor opened with prefilled session title");
    inject_keys_paced(&mut harness, b"LEFT RIGHT");
    harness.inject_keys(ALT_LEFT).expect("iTerm Alt+Left");
    inject_keys_paced(&mut harness, b"MID");
    harness.inject_keys(ALT_RIGHT).expect("iTerm Alt+Right");
    inject_keys_paced(&mut harness, b"END");
    harness
        .wait_for_text(
            &format!("rename: {ROW_TITLE}LEFT MIDRIGHTEND"),
            Duration::from_secs(10),
        )
        .expect("raw Alt arrows changed rename draft");
    harness.inject_keys(keys::ENTER).expect("commit rename");
    wait_for_labels_absent(&mut harness, &["rename:"], Duration::from_secs(10));
    harness
        .wait_for_text(
            &format!("{ROW_TITLE}LEFT MIDRIGHTEND"),
            Duration::from_secs(10),
        )
        .expect("committed dashboard rename visible");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.inject_keys(b"\x11").expect("Ctrl+Q arm");
    harness
        .wait_for_text("press again to quit", Duration::from_secs(10))
        .expect("quit confirmation rendered");
    harness.inject_keys(b"\x11").expect("Ctrl+Q confirm");
    assert_eq!(
        wait_for_exit_status(&mut harness, Duration::from_secs(10)).expect("wait for pager exit"),
        PtyExitPoll::Exited(0),
        "pager must exit cleanly"
    );
}
