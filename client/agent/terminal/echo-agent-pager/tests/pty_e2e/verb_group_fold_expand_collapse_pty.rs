// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const DONE_SENTINEL: &str = "VERB_GROUP_DONE";

/// PTY: runs of consecutive reads/searches fold into one "Verb N noun" header
/// row and an Edit stays a standalone separator. Double-click expands the
/// group to individual rows — including member 0 below the header — where the
/// selected slot acts as member 0: Right (Expand; `l` is its vim-mode alias)
/// opens the member's own block, Left (Collapse; `h` in vim mode) closes it
/// and then collapses the whole group.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn verb_group_fold_expand_collapse_pty() {
    let content = ContentController::start().await.expect("start content");
    // Pin ON via the config tier so the test doesn't ride the client default.
    seed_ui_config(&content, "group_tool_verbs = true");

    // Seed real files under the isolated HOME so the reads/edit succeed.
    let mut paths = Vec::new();
    for name in [
        "a1.txt",
        "a2.txt",
        "a3.txt",
        "edit_me.txt",
        "b1.txt",
        "b2.txt",
    ] {
        let path = content.home().join(name);
        // a1's body is unique so opening MEMBER 0's block below has an
        // unambiguous sentinel (the edit diff already shows "hello verb
        // group" on screen).
        let body = if name == "a1.txt" {
            "first member body a1\n"
        } else {
            "hello verb group\n"
        };
        std::fs::write(&path, body).expect("write fixture file");
        paths.push(dunce::canonicalize(&path).unwrap_or(path));
    }
    fn read_args(p: &std::path::Path) -> String {
        json!({ "target_file": p.to_string_lossy() }).to_string()
    }
    let home_str = content.home().to_string_lossy().into_owned();

    // read x3 → grep x2 → edit → read x2, then a plain completion to settle.
    let _tool_turns = [
        expect_tool_turn(&content, "call_r1", "read_file", read_args(&paths[0])),
        expect_tool_turn(&content, "call_r2", "read_file", read_args(&paths[1])),
        expect_tool_turn(&content, "call_r3", "read_file", read_args(&paths[2])),
        expect_tool_turn(
            &content,
            "call_g1",
            "grep",
            json!({ "pattern": "hello", "path": home_str }).to_string(),
        ),
        expect_tool_turn(
            &content,
            "call_g2",
            "grep",
            json!({ "pattern": "verb", "path": home_str }).to_string(),
        ),
        expect_tool_turn(
            &content,
            "call_e1",
            "search_replace",
            json!({
                "file_path": paths[3].to_string_lossy(),
                "old_string": "hello verb group",
                "new_string": "hola verb group",
            })
            .to_string(),
        ),
        expect_tool_turn(&content, "call_r4", "read_file", read_args(&paths[4])),
        expect_tool_turn(&content, "call_r5", "read_file", read_args(&paths[5])),
    ];
    content.set_response(DONE_SENTINEL);

    let binary = pager_binary().expect("resolve pager binary");
    // --yolo auto-approves the scripted write tool (mirrors the background-task
    // e2e, the other tool-running PTY test).
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

    // The final completion means every tool turn ran; labels are past tense.
    harness
        .wait_for_text(DONE_SENTINEL, Duration::from_secs(90))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled transcript; got:\n{}",
                harness.screen_contents()
            )
        });

    // First run folds reads + greps into one header ("· N failed" may follow).
    harness
        .wait_for_text("Read 3 files, Searched 2 patterns", Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "expected first verb-group header; got:\n{}",
                harness.screen_contents()
            )
        });
    // Second run folds independently; the edit stays a standalone row between.
    assert!(
        harness.contains_text("Read 2 files"),
        "expected second verb-group header\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.contains_text("edit_me.txt"),
        "edit must stay a standalone separator row\nscreen:\n{}",
        harness.screen_contents()
    );
    // Members of a folded run are hidden.
    assert!(
        !harness.contains_text("a2.txt"),
        "folded member must be hidden\nscreen:\n{}",
        harness.screen_contents()
    );

    // Focus scrollback with a single Tab, then wait for a scrollback-only
    // footer hint to prove the scrollback owns keys (Tab TOGGLES focus;
    // polling the render avoids racing turn-finish redraws). The thinking
    // fold chord renders whenever the scrollback is focused, regardless of
    // which row type is selected — unlike "Space:prompt", which higher
    // priority selection hints can crowd out.
    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Ctrl+e:", Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "scrollback must own keys before mouse interaction; got:\n{}",
                harness.screen_contents()
            )
        });
    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, "Read 3 files").unwrap_or_else(|| {
        panic!("could not locate verb-group header; screen:\n{screen}");
    });
    // The transcript is short (fits the viewport), so expanding must grow
    // members downward in place: the header's screen row may not move.
    let header_row_before = row;
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
    // Expanded shape: the header line sits ABOVE the members and every
    // member — including the first — renders as its own row. The slot stays
    // selected and acts as MEMBER 0: the caret sits on the member row
    // pointing right, and the header row wears no caret.
    harness
        .wait_for_text("a1.txt", Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "expected the FIRST member row while expanded; got:\n{}",
                harness.screen_contents()
            )
        });
    for member in ["a2.txt", "a3.txt"] {
        assert!(
            harness.contains_text(member),
            "all members must render while expanded ({member})\nscreen:\n{}",
            harness.screen_contents()
        );
    }
    assert!(
        harness.contains_text("◈ Read 3 files"),
        "expanded header row must render caret-free chrome\nscreen:\n{}",
        harness.screen_contents()
    );
    let screen = harness.screen_contents();
    let expanded_row = locate_screen_text(&screen, "Read 3 files")
        .unwrap_or_else(|| panic!("expanded header must stay on screen:\n{screen}"))
        .0;
    assert_eq!(
        expanded_row, header_row_before,
        "expanding must not move the header's screen row\nscreen:\n{screen}"
    );
    // Caret on member 0. Collapsed Read headers show basename only, so the
    // path may be `a1.txt` rather than an absolute `/…` prefix.
    assert!(
        harness.contains_text("› Read a1.txt") || harness.contains_text("› Read /"),
        "member 0's row must carry the selection caret\nscreen:\n{}",
        harness.screen_contents()
    );

    // Prove the scrollback owns keys, then Right: it must open MEMBER 0's
    // own block (not re-toggle the group). Opening drops member 0 from the
    // run, so the group dissolves while its body is on show.
    harness
        .wait_for_text("Ctrl+e:", Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "scrollback must own keys before Right; got:\n{}",
                harness.screen_contents()
            )
        });
    harness.inject_keys(b"\x1b[C").expect("open member 0");
    harness
        .wait_for_text("first member body a1", Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "Right must open member 0's block; got:\n{}",
                harness.screen_contents()
            )
        });

    // Left closes the block; the still-expanded group re-forms around it.
    harness.inject_keys(b"\x1b[D").expect("close member 0");
    harness
        .wait_for_text("◈ Read 3 files", Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "closing member 0 must re-form the expanded group; got:\n{}",
                harness.screen_contents()
            )
        });
    assert!(
        !harness.contains_text("first member body a1"),
        "member 0's body must fold away again\nscreen:\n{}",
        harness.screen_contents()
    );

    // A second Left from the slot collapses the whole group (member path).
    harness.inject_keys(b"\x1b[D").expect("collapse group");
    harness.update(Duration::from_millis(500));
    for member in ["a1.txt", "a2.txt"] {
        assert!(
            !harness.contains_text(member),
            "members must fold back after Left ({member})\nscreen:\n{}",
            harness.screen_contents()
        );
    }
    // Collapse re-selects the header: the caret overdraws the diamond (not
    // the label) and points right again in the collapsed state.
    assert!(
        harness.contains_text("› Read 3 files, Searched 2 patterns"),
        "collapsed header must return with the right-pointing caret\nscreen:\n{}",
        harness.screen_contents()
    );
    let screen = harness.screen_contents();
    let collapsed_row = locate_screen_text(&screen, "Read 3 files")
        .unwrap_or_else(|| panic!("collapsed header must stay on screen:\n{screen}"))
        .0;
    assert_eq!(
        collapsed_row, header_row_before,
        "collapsing must not move the header's screen row\nscreen:\n{screen}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
