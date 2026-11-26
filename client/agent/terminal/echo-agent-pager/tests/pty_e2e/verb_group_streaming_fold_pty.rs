// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const DONE_SENTINEL: &str = "VERB_GROUP_STREAM_DONE";

/// PTY: verb-group folding happens IN PLACE while the run is still streaming.
/// With three scripted read turns paced by `set_chunk_delay` (which scripted
/// SSE honors), the header exists from the FIRST read on (a singleton run
/// already folds, so no fold-in jump when the second arrives), the "Read 2
/// files" label must be on screen while the third turn is still in flight —
/// before the 3-file label or the final completion exists — and the settled
/// transcript then shows the final "Read 3 files".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn verb_group_streaming_fold_pty() {
    let content = ContentController::start().await.expect("start content");
    // Pin ON via the config tier so the test doesn't ride the client default.
    seed_ui_config(&content, "group_tool_verbs = true");

    // Seed real files under the isolated HOME so the reads succeed.
    let mut paths = Vec::new();
    for name in ["s1.txt", "s2.txt", "s3.txt"] {
        let path = content.home().join(name);
        std::fs::write(&path, "hello verb group\n").expect("write fixture file");
        paths.push(dunce::canonicalize(&path).unwrap_or(path));
    }
    let _tool_turns: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            expect_tool_turn(
                &content,
                &format!("call_s{i}"),
                "read_file",
                json!({ "target_file": p.to_string_lossy() }).to_string(),
            )
        })
        .collect();
    content.set_response(DONE_SENTINEL);
    // Hold each scripted turn open (4 SSE events x 350ms ≈ 1.4s) so the
    // mid-flight window is pollable; cleared after capture so the tail
    // settles fast.
    content.set_chunk_delay(Some(Duration::from_millis(350)));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // Mid-flight: the FIRST read already folds into a singleton header
    // ("Reading 1 file" / "Read 1 file"), then two completed reads keep the
    // same header while turn 3 streams. The negative conditions pin "still in
    // flight" — if the poll only ever saw the settled screen, the DONE break
    // trips and the asserts fail.
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut saw_singleton = false;
    let mut saw_midflight = false;
    while Instant::now() < deadline {
        harness.update(Duration::from_millis(30));
        let s = harness.screen_contents();
        if (s.contains("Reading 1 file") || s.contains("Read 1 file")) && !s.contains("2 files") {
            saw_singleton = true;
        }
        if s.contains("Read 2 files") && !s.contains("Read 3 files") && !s.contains(DONE_SENTINEL) {
            saw_midflight = true;
            break;
        }
        if s.contains(DONE_SENTINEL) {
            break;
        }
    }
    content.set_chunk_delay(None);
    assert!(
        saw_singleton,
        "the header must appear with the first read (singleton fold)\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        saw_midflight,
        "fold header must exist while the run is still in flight\nscreen:\n{}",
        harness.screen_contents()
    );

    harness
        .wait_for_text(DONE_SENTINEL, Duration::from_secs(60))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled transcript; got:\n{}",
                harness.screen_contents()
            )
        });
    harness
        .wait_for_text("Read 3 files", Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "final past-tense header missing; got:\n{}",
                harness.screen_contents()
            )
        });
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
