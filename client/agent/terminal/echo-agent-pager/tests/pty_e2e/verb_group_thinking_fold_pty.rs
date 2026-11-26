// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const DONE_SENTINEL: &str = "VERB_GROUP_THINKING_DONE";

/// Word unique to the reasoning body: never in the answer, the fixture files,
/// or any chrome, so its presence pins exactly "the thinking body is on
/// screen".
const REASONING_SENTINEL: &str = "THOUGHTFOLDSENTINEL";

/// Collapsed thought member/header text ("Thought for Xs").
const THOUGHT_HEADER: &str = "Thought for";

/// PTY: a thought folds into the verb-group run it precedes. The scripted
/// turn thinks, then reads twice: while the reasoning streams its live panel
/// is on screen; once the run settles the group shows only the tools-only
/// "Read 2 files" header (the collapsed thought row folded away); expanding
/// the group reveals the thought as its own 1-row member.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn verb_group_thinking_fold_pty() {
    let content = ContentController::start().await.expect("start content");
    // Both opt-ins are explicit so the test doesn't ride rollout defaults:
    // folding is the feature under test, and thinking must be shown for
    // thought entries to exist at all (ingestion gate).
    seed_ui_config(
        &content,
        "group_tool_verbs = true\nshow_thinking_blocks = true",
    );

    // Seed real files under the isolated HOME so the reads succeed.
    let mut paths = Vec::new();
    for name in ["t1.txt", "t2.txt"] {
        let path = content.home().join(name);
        std::fs::write(&path, "hello thought fold\n").expect("write fixture file");
        paths.push(dunce::canonicalize(&path).unwrap_or(path));
    }
    let reasoning = format!("{REASONING_SENTINEL} weighing which fixture file to read first");

    // Turn 1 thinks and calls the first read: the tool call finishes the
    // thought, which auto-collapses and folds into the forming run.
    let args0 = json!({ "target_file": paths[0].to_string_lossy() }).to_string();
    let _thinking_turn = content.expect_agent_turn_with_responses(
        "thinking then first read",
        ScriptedResponse::sse(sse::responses_api_reasoning_then_tool_call_events(
            &reasoning,
            "call_t0",
            "read_file",
            &args0,
            "test-model",
        )),
        ScriptedResponse::sse(sse::chat_completions_reasoning_then_tool_call_events(
            &reasoning,
            "call_t0",
            "read_file",
            &args0,
            "test-model",
        )),
    );
    // Turn 2: a plain second read grows the already-folded run to two tools.
    let args1 = json!({ "target_file": paths[1].to_string_lossy() }).to_string();
    let _second_read_turn = expect_tool_turn(&content, "call_t1", "read_file", args1);
    content.set_response(DONE_SENTINEL);
    // Pace the scripted SSE so the streaming-thinking window is pollable;
    // cleared after capture so the tail settles fast.
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

    // While the reasoning streams, its live panel shows the body. The DONE
    // break pins "still in flight": a poll that only ever saw the settled
    // screen (where the body is folded away) fails the assert below.
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut saw_streaming = false;
    while Instant::now() < deadline {
        harness.update(Duration::from_millis(30));
        let s = harness.screen_contents();
        if s.contains(REASONING_SENTINEL) && !s.contains(DONE_SENTINEL) {
            saw_streaming = true;
            break;
        }
        if s.contains(DONE_SENTINEL) {
            break;
        }
    }
    content.set_chunk_delay(None);
    assert!(
        saw_streaming,
        "thinking body must render while the reasoning streams\nscreen:\n{}",
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
        .wait_for_text("Read 2 files", Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "tools-only verb-group header missing; got:\n{}",
                harness.screen_contents()
            )
        });
    // The finished thought folded into the group: no standalone thought row
    // and no reasoning body anywhere on the settled screen.
    assert!(
        !harness.contains_text(THOUGHT_HEADER),
        "folded thought must not keep a standalone row\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text(REASONING_SENTINEL),
        "reasoning body must not leak into the settled transcript\nscreen:\n{}",
        harness.screen_contents()
    );

    // Expand the group: Tab focuses scrollback (scrollback-only footer hint
    // proves key ownership), then a double-click on the header toggles it.
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
    let (row, col) = locate_screen_text(&screen, "Read 2 files").unwrap_or_else(|| {
        panic!("could not locate verb-group header; screen:\n{screen}");
    });
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

    // Expanded: the thought reveals as its own collapsed 1-row member
    // ("Thought for Xs") — still just the header line, not the body.
    harness
        .wait_for_text(THOUGHT_HEADER, Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "expanded group must reveal the thought member row; got:\n{}",
                harness.screen_contents()
            )
        });
    assert!(
        !harness.contains_text(REASONING_SENTINEL),
        "the member row is the collapsed header, not the body\nscreen:\n{}",
        harness.screen_contents()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
