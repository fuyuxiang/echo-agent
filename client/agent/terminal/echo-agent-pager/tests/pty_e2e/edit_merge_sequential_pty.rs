// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

const DONE_SENTINEL: &str = "EDIT_MERGE_SEQ_DONE";

/// Agent text between the merged run and the break-case edit.
const BREAK_TEXT_SENTINEL: &str = "MERGE_BREAK_TEXT";

/// Markers planted by the scripted edits: visible only in the diff body.
const EDIT_ONE_MARK: &str = "EDIT_ONE_MARK";
const EDIT_THREE_MARK: &str = "EDIT_THREE_MARK";

const FIXTURE: &str = "merge_fix.py";

/// One line of the fixture per word, so each edit is a 1:1 line replacement
/// (`+1/-1`) and line numbers stay stable across the whole scripted turn.
fn fixture_text() -> String {
    let words = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliett", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo",
        "sierra", "tango", "uniform", "victor", "whiskey", "xray", "yankee", "zulu", "omega",
        "finale",
    ];
    let mut text = String::from("# merge fixture\n");
    for (i, word) in words.iter().enumerate() {
        text.push_str(&format!("v{:02} = \"{word}\"\n", i + 1));
    }
    text
}

fn edit_args(abs: &Path, var: &str, word: &str, mark: &str) -> String {
    json!({
        "file_path": abs.to_string_lossy(),
        "old_string": format!("{var} = \"{word}\""),
        "new_string": format!("{var} = \"{word}\"  # {mark}"),
    })
    .to_string()
}

/// Count screen rows carrying an Edit header for the fixture.
fn edit_header_rows(screen: &str) -> usize {
    screen
        .lines()
        .filter(|l| l.contains(&format!("Edit {FIXTURE}")))
        .count()
}

/// PTY: with `collapsed_edit_blocks` enabled, three sequential same-file
/// edits coalesce into ONE Edit row whose header sums the diffstat (`+3/-3`);
/// expanding it shows every hunk with `… N unchanged lines` gap markers
/// between them. A fourth edit arriving after intervening agent text stays a
/// separate second Edit row — counted after wheeling back above the second
/// submit's page-flip, which pins the new prompt to the pane top and scrolls
/// turn 1 out of view by design. (Flag off, coalescing is disabled entirely —
/// pinned by the tracker unit test.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run with cargo test -p echo-agent-pager --test pty_e2e -- --ignored"]
async fn edit_merge_sequential_pty() {
    let content = ContentController::start().await.expect("start content");
    seed_ui_config(&content, "collapsed_edit_blocks = true");

    let target = content.home().join(FIXTURE);
    std::fs::write(&target, fixture_text()).expect("write fixture");
    let abs = dunce::canonicalize(&target).unwrap_or(target.clone());

    // Three 1:1 replacements at widely separated, increasing lines so every
    // merged-hunk gap is computable (edits sit ~11 lines apart, context ±3).
    let _edit_turns: [AgentTurnExpectation; 3] = [
        expect_tool_turn(
            &content,
            "call_sr_1",
            "search_replace",
            edit_args(&abs, "v03", "charlie", EDIT_ONE_MARK),
        ),
        expect_tool_turn(
            &content,
            "call_sr_2",
            "search_replace",
            edit_args(&abs, "v14", "november", "EDIT_TWO_MARK"),
        ),
        expect_tool_turn(
            &content,
            "call_sr_3",
            "search_replace",
            edit_args(&abs, "v25", "yankee", EDIT_THREE_MARK),
        ),
    ];
    // The first prompt's turn ends on agent text — the break for the run.
    let break_text = format!("{BREAK_TEXT_SENTINEL} first batch settled.");
    let _break_turn: AgentTurnExpectation =
        content.expect_agent_turn("first batch settled", &break_text);
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
    harness
        .wait_for_text(BREAK_TEXT_SENTINEL, Duration::from_secs(90))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled first batch; got:\n{}",
                harness.screen_contents()
            )
        });

    // One merged row with the summed diffstat; the per-call counts are gone.
    harness
        .wait_for_text(&format!("Edit {FIXTURE} +3/-3"), Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "expected one merged `Edit {FIXTURE} +3/-3` row; got:\n{}",
                harness.screen_contents()
            )
        });
    let screen = harness.screen_contents();
    assert_eq!(
        edit_header_rows(&screen),
        1,
        "three adjacent edits must render as ONE Edit row\nscreen:\n{screen}"
    );
    assert!(
        !screen.contains("+1/-1") && !screen.contains("+2/-2"),
        "per-call diffstats must not survive the merge\nscreen:\n{screen}"
    );

    // Expand the merged block: all hunks with gap markers between them.
    let (row, col) = locate_screen_text(&screen, &format!("Edit {FIXTURE}")).unwrap_or_else(|| {
        panic!("could not locate the merged Edit header; screen:\n{screen}");
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
        .wait_for_text(" unchanged lines", Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "expanded merged block must show gap markers; got:\n{}",
                harness.screen_contents()
            )
        });
    assert!(
        harness.contains_text(EDIT_ONE_MARK) && harness.contains_text(EDIT_THREE_MARK),
        "expanded body must include hunks from the first AND last merged edit\nscreen:\n{}",
        harness.screen_contents()
    );

    // Fold the block back so the final screen keeps every row visible.
    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, &format!("Edit {FIXTURE}")).unwrap_or_else(|| {
        panic!("could not re-locate the merged Edit header; screen:\n{screen}");
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
        .expect("double-click to fold");
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.contains_text(EDIT_ONE_MARK) {
        assert!(
            Instant::now() < deadline,
            "merged block must fold back\nscreen:\n{}",
            harness.screen_contents()
        );
        harness.update(Duration::from_millis(100));
    }

    // BREAK case: a fourth same-file edit after the agent-text turn stays a
    // separate row instead of merging across the visible text entry.
    harness
        .wait_for_turn_idle(Duration::from_secs(15))
        .expect("turn idle after break-case edit");
    let _edit_four = expect_tool_turn(
        &content,
        "call_sr_4",
        "search_replace",
        edit_args(&abs, "v10", "juliett", "EDIT_FOUR_MARK"),
    );
    harness
        .inject_keys(b"go again\r")
        .expect("submit second prompt");
    harness
        .wait_for_text(DONE_SENTINEL, Duration::from_secs(90))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled second turn; got:\n{}",
                harness.screen_contents()
            )
        });
    harness
        .wait_for_text(&format!("Edit {FIXTURE} +1/-1"), Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "the post-break edit must keep its own `+1/-1` row; got:\n{}",
                harness.screen_contents()
            )
        });
    // The second submit page-flipped the viewport (the new prompt pins to the
    // pane top — dispatch/queue.rs `scroll_to_entry_top` +
    // `enable_follow_with_preserve`), so turn 1 legitimately sits above the
    // fold. Wheel back to the transcript top — the whole thing fits one
    // screen — before counting rows across both turns.
    send_wheel_burst(
        &mut harness,
        SGR_SCROLL_UP,
        30,
        WHEEL_ROW,
        WHEEL_COL,
        Duration::ZERO,
    );
    harness
        .wait_for_text(&format!("Edit {FIXTURE} +3/-3"), Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "the merged row must scroll back into view with its summed \
                 diffstat intact; got:\n{}",
                harness.screen_contents()
            )
        });
    let screen = harness.screen_contents();
    assert_eq!(
        edit_header_rows(&screen),
        2,
        "expected the merged row plus one separate post-break row\nscreen:\n{screen}"
    );
    assert!(
        screen.contains("+1/-1"),
        "the post-break row keeps its own diffstat\nscreen:\n{screen}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    #[cfg(unix)]
    write_cast_if_requested(&harness, "edit_merge_sequential.cast");

    harness.quit().expect("clean quit");
}
