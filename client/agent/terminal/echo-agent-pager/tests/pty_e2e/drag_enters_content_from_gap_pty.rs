// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Single-line message; the drag enters it at the tail word.
const GAPDEEP_LINE: &str = "GAPDEEP alpha beta gamma delta epsilon";

const ENTRY_WORD: &str = "epsilon";

/// Wait until the post-turn layout is stable enough that mouse coordinates
/// from `locate_screen_text` still match the next frame. Turn-end relayout
/// (spinner/hint teardown) used to shift rows between a fixed 1500ms settle
/// and the SGR burst under `--test-threads=4`.
fn wait_for_stable_gap_layout(harness: &mut PtyHarness) -> (u16, u16, u16) {
    let mut last: Option<(u16, u16, u16)> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        harness.update(Duration::from_millis(100));
        let screen = harness.screen_contents();
        let Some((msg_row, _)) = locate_screen_text(&screen, "GAPDEEP") else {
            last = None;
            if Instant::now() >= deadline {
                panic!("could not locate GAPDEEP; screen:\n{screen}");
            }
            continue;
        };
        let Some((entry_row, entry_col)) = locate_screen_text(&screen, ENTRY_WORD) else {
            last = None;
            if Instant::now() >= deadline {
                panic!("could not locate {ENTRY_WORD:?}; screen:\n{screen}");
            }
            continue;
        };
        let Some((marker_row, _)) = locate_screen_text(&screen, "Worked for") else {
            last = None;
            if Instant::now() >= deadline {
                panic!("could not locate the turn marker; screen:\n{screen}");
            }
            continue;
        };
        if entry_row != msg_row || marker_row <= msg_row {
            last = None;
            if Instant::now() >= deadline {
                panic!(
                    "setup: message must be a single unwrapped line above the turn marker\n\
                     screen:\n{screen}"
                );
            }
            continue;
        }
        let gap_row = marker_row + 1;
        let gap_line = screen.lines().nth(gap_row as usize).unwrap_or("");
        if !gap_line.trim().is_empty() {
            last = None;
            if Instant::now() >= deadline {
                panic!("setup: the press row must be a blank gap; line: {gap_line:?}");
            }
            continue;
        }
        let coords = (entry_row, entry_col, gap_row);
        if last == Some(coords) {
            return coords;
        }
        last = Some(coords);
        if Instant::now() >= deadline {
            panic!("layout never stabilized for gap-drag coords; last={last:?}\nscreen:\n{screen}");
        }
    }
}

/// PTY: a mouse-down on the blank gap below the conversation (between the
/// turn marker and the prompt box) arms an anchor-less drag — dead space
/// is a valid drag start — and the anchor materializes at the first drag
/// position that lands on selectable text: here a word inside the last
/// message. The payload is the entry-to-release slice of that single line
/// — not a snap to the press-nearest text, not a block copy.
///
/// `SSH_CONNECTION` forces the OSC 52 clipboard route for readback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn drag_enters_content_from_gap_pty() {
    let content = ContentController::start().await.expect("start content");
    // Copy-on-release (OSC 52) only in flash. Pin it so suite siblings that
    // seed hold/word_select cannot change semantics if config leaks.
    seed_ui_config(&content, "keep_text_selection = \"flash\"");
    content.set_response(GAPDEEP_LINE.to_string());

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
        .wait_for_text(ENTRY_WORD, Duration::from_secs(45))
        .expect("message rendered");
    harness
        .wait_for_text("Worked for", Duration::from_secs(20))
        .expect("turn marker rendered");
    harness
        .wait_for_turn_idle(Duration::from_secs(20))
        .expect("turn idle before locating gap-drag coords");

    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(10))
        .expect("scrollback focused (Space:prompt hint) after Tab");

    let (entry_row, entry_col, gap_row) = wait_for_stable_gap_layout(&mut harness);

    // PRESS in the gap, then drag up into the message. The motion samples
    // jump the marker row deliberately (terminals coalesce motion): the
    // same-row column clamp makes the marker's line hittable at any column
    // of its row, so a sample there would anchor the drag on the marker —
    // the correct first-text-entered answer for that path, but not this
    // test's subject. First sample on the message = anchor at the word's
    // first column; then extend to its last column and release.
    let head_col = entry_col + ENTRY_WORD.len() as u16 - 1;
    let seen = decode_osc52_payloads(harness.raw_output()).len();
    let mut drag = String::new();
    drag.push_str(&sgr_mouse(0, gap_row, entry_col, 'M'));
    drag.push_str(&sgr_mouse(32, entry_row, entry_col, 'M'));
    drag.push_str(&sgr_mouse(32, entry_row, head_col, 'M'));
    drag.push_str(&sgr_mouse(0, entry_row, head_col, 'm'));
    harness
        .inject_keys(drag.as_bytes())
        .expect("press the gap, drag up into the message");

    let deadline = Instant::now() + Duration::from_secs(10);
    let payloads = loop {
        harness.update(Duration::from_millis(200));
        let all = decode_osc52_payloads(harness.raw_output());
        if all.len() > seen || Instant::now() >= deadline {
            break all.into_iter().skip(seen).collect::<Vec<_>>();
        }
    };
    assert!(
        !payloads.is_empty(),
        "expected an OSC 52 clipboard write after release; screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    assert_eq!(
        joined, ENTRY_WORD,
        "payload must be the entry-to-release slice of the entered line \
         (anchor at text entry, not at the press or a snap); payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
