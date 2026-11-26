// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const DONE_SENTINEL: &str = "EDIT_MERGE_PAR_DONE";

const FIXTURE: &str = "parallel_fix.py";

/// PTY: with `collapsed_edit_blocks` enabled, TWO search_replace calls to the
/// same file issued in ONE model turn (parallel tool calls) coalesce into a
/// single Edit row with the summed diffstat, regardless of the order their
/// completions land in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run with cargo test -p echo-agent-pager --test pty_e2e -- --ignored"]
async fn edit_merge_parallel_pty() {
    let content = ContentController::start().await.expect("start content");
    seed_ui_config(&content, "collapsed_edit_blocks = true");

    let target = content.home().join(FIXTURE);
    std::fs::write(
        &target,
        "# parallel merge fixture\n\
         first = \"one\"\n\
         second = \"two\"\n\
         third = \"three\"\n\
         fourth = \"four\"\n\
         fifth = \"five\"\n\
         sixth = \"six\"\n\
         seventh = \"seven\"\n\
         eighth = \"eight\"\n\
         ninth = \"nine\"\n\
         tenth = \"ten\"\n",
    )
    .expect("write fixture");
    let abs = dunce::canonicalize(&target).unwrap_or(target.clone());

    // Non-overlapping 1:1 replacements so both calls succeed against the
    // same starting file whatever order the shell runs them in.
    let args_a = json!({
        "file_path": abs.to_string_lossy(),
        "old_string": "second = \"two\"",
        "new_string": "second = \"two\"  # PAR_EDIT_A",
    })
    .to_string();
    let args_b = json!({
        "file_path": abs.to_string_lossy(),
        "old_string": "ninth = \"nine\"",
        "new_string": "ninth = \"nine\"  # PAR_EDIT_B",
    })
    .to_string();
    enqueue_parallel_tool_turn(
        &content,
        &[
            ("call_par_1", "search_replace", args_a),
            ("call_par_2", "search_replace", args_b),
        ],
    );
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
        .wait_for_text(DONE_SENTINEL, Duration::from_secs(90))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled transcript; got:\n{}",
                harness.screen_contents()
            )
        });

    // One merged row summing both calls' diffstats.
    harness
        .wait_for_text(&format!("Edit {FIXTURE} +2/-2"), Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "expected one merged `Edit {FIXTURE} +2/-2` row; got:\n{}",
                harness.screen_contents()
            )
        });
    let screen = harness.screen_contents();
    let edit_rows = screen
        .lines()
        .filter(|l| l.contains(&format!("Edit {FIXTURE}")))
        .count();
    assert_eq!(
        edit_rows, 1,
        "parallel same-file edits must render as ONE Edit row\nscreen:\n{screen}"
    );
    assert!(
        !screen.contains("+1/-1"),
        "per-call diffstats must not survive the merge\nscreen:\n{screen}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
