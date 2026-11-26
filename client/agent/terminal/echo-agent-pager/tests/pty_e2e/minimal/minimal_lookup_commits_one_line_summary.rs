// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

const BODY_SENTINEL: &str = "READBODYONLYSENTINEL";
const DONE_SENTINEL: &str = "LOOKUP_TURN_DONE";

/// Uses `read_file` rather than `grep`: the grep tool shells out to `rg`,
/// which is absent from the Bazel remote-exec sandbox (only echo-agent-tools'
/// own test targets ship `@ripgrep_hermetic//:rg`), and a failed spawn
/// degrades to a zero-match result that vacuously passes the absence assert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_lookup_commits_one_line_summary() {
    let content = ContentController::start().await.expect("start content");

    let fixture = content.home().join("haystack.txt");
    std::fs::write(&fixture, format!("{BODY_SENTINEL} body line\n")).expect("write fixture");

    let _read_turn = expect_tool_turn(
        &content,
        "call_read",
        "read_file",
        json!({ "target_file": fixture.to_string_lossy() }).to_string(),
    );
    content.set_response(DONE_SENTINEL);

    let mut harness = spawn_minimal_in_dir(
        &content,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &["--yolo", "--trust"],
        content.home(),
    );
    wait_minimal_ready(&mut harness);

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_full_text(DONE_SENTINEL, Duration::from_secs(60))
        .expect("tool turn settles");
    harness
        .wait_for_text(MINIMAL_IDLE_SENTINEL, Duration::from_secs(20))
        .expect("return to idle");

    harness
        .wait_for_full_text("haystack.txt", Duration::from_secs(10))
        .expect("read header committed");
    assert!(
        !harness.full_text().contains(BODY_SENTINEL),
        "successful read must commit as a one-line header, without file \
         content\nfull:\n{}",
        harness.full_text()
    );

    harness.inject_keys(b"\x05").expect("ctrl+e expand");
    harness
        .wait_for_full_text(BODY_SENTINEL, Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "Ctrl+E must re-print the read with its file content: {e}\nfull:\n{}",
                harness.full_text()
            )
        });

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
