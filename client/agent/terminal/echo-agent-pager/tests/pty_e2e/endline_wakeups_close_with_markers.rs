//! PTY: a turn ends with three flag-gated background commands running; each
//! released flag lands a completion chip, the auto-wake reply, and that wake
//! turn's own closing marker. Asserts the positional chain marker < chip <
//! reply per round — FOUR "Worked for" total — and the cue counting 3→2→1→gone.
#[allow(unused_imports)]
use super::common::*;

/// One flag-gated background command per released flag file.
#[cfg(unix)]
const TASKS: usize = 3;

/// Taller than [`DEFAULT_ROWS`]: the full chain (initial turn + three wake
/// turns) must stay on screen at once for the positional asserts.
#[cfg(unix)]
const ROWS: u16 = 70;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn endline_wakeups_close_with_markers() {
    let content = ContentController::start().await.expect("start content");
    let flags: Vec<std::path::PathBuf> = (0..TASKS)
        .map(|i| content.home().join(format!("endline_status_flag_{i}")))
        .collect();

    // The turn backgrounds one flag-gated command per tool call…
    let _background_turns: Vec<_> = flags
        .iter()
        .enumerate()
        .map(|(i, flag)| {
            let args = json!({
                "command": format!(
                    "while [ ! -e {} ]; do /bin/sleep 0.2; done",
                    flag.display()
                ),
                "description": format!("flag-gated command {i}"),
                "is_background": true
            })
            .to_string();
            expect_tool_turn(
                &content,
                &format!("call_endline_status_{i}"),
                "run_terminal_command",
                args,
            )
        })
        .collect();
    // …then a text response ends it with all three still running, followed by
    // one response for each auto-wake.
    let _settled_turn = content.expect_agent_turn("initial settled turn", "STATUS_TURN_SETTLED");
    let _wake_one = content.expect_agent_turn("first completion wake", "WAKE_REPLY_ONE");
    let _wake_two = content.expect_agent_turn("second completion wake", "WAKE_REPLY_TWO");
    let _wake_three = content.expect_agent_turn("third completion wake", "WAKE_REPLY_THREE");
    content.set_response("STATUS_FALLBACK");

    let binary = pager_binary().expect("resolve pager binary");
    // --yolo skips the bash permission prompt; --trust skips the folder-trust gate.
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        ROWS,
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
        .wait_for_text("STATUS_TURN_SETTLED", Duration::from_secs(60))
        .unwrap_or_else(|_| {
            panic!(
                "turn never settled; screen:\n{}\n--- non-system messages ---\n{}",
                harness.screen_contents(),
                dump_non_system_messages(&content.request_bodies())
            )
        });
    harness
        .wait_for_text("Worked for", Duration::from_secs(30))
        .unwrap_or_else(|_| {
            panic!(
                "the end marker never appeared; screen:\n{}",
                harness.screen_contents()
            )
        });
    harness
        .wait_for_text("3 commands still running", Duration::from_secs(30))
        .unwrap_or_else(|_| {
            panic!(
                "the watching cue never showed the running count; screen:\n{}",
                harness.screen_contents()
            )
        });

    std::fs::write(&flags[0], b"done").expect("release flag 0");
    let wake_one = wait_until(Duration::from_secs(45), || {
        harness.update(Duration::from_millis(100));
        let screen = harness.screen_contents();
        screen.contains("WAKE_REPLY_ONE")
            && screen.matches("Worked for").count() == 2
            && screen.contains("2 commands still running")
    });
    assert!(
        wake_one,
        "expected chip → wake reply → the wake's closing marker, watching cue at 2; screen:\n{}",
        harness.screen_contents()
    );

    std::fs::write(&flags[1], b"done").expect("release flag 1");
    let wake_two = wait_until(Duration::from_secs(45), || {
        harness.update(Duration::from_millis(100));
        let screen = harness.screen_contents();
        screen.contains("WAKE_REPLY_TWO")
            && screen.matches("Worked for").count() == 3
            && screen.contains("1 command still running")
    });
    assert!(
        wake_two,
        "expected the second wake chain (with its marker) below the earlier lines; screen:\n{}",
        harness.screen_contents()
    );

    std::fs::write(&flags[2], b"done").expect("release flag 2");
    let wake_three = wait_until(Duration::from_secs(45), || {
        harness.update(Duration::from_millis(100));
        let screen = harness.screen_contents();
        screen.contains("WAKE_REPLY_THREE")
            && screen.matches("Worked for").count() == 4
            && !screen.contains("still running")
    });
    assert!(
        wake_three,
        "the last chatty wake must close with its marker and retire the watching cue; screen:\n{}",
        harness.screen_contents()
    );

    // Screen text is row-major, so match offsets order the lines.
    let screen = harness.screen_contents();
    let chips: Vec<usize> = screen
        .match_indices("Task completed")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        chips.len(),
        TASKS,
        "one completion chip per task; screen:\n{screen}"
    );
    let markers: Vec<usize> = screen.match_indices("Worked for").map(|(i, _)| i).collect();
    assert_eq!(
        markers.len(),
        4,
        "four markers — the user turn's plus one per chatty wake; screen:\n{screen}"
    );
    let w1 = screen.find("WAKE_REPLY_ONE").expect("wake reply 1");
    let w2 = screen.find("WAKE_REPLY_TWO").expect("wake reply 2");
    let w3 = screen.find("WAKE_REPLY_THREE").expect("wake reply 3");
    assert!(
        markers[0] < chips[0]
            && chips[0] < w1
            && w1 < markers[1]
            && markers[1] < chips[1]
            && chips[1] < w2
            && w2 < markers[2]
            && markers[2] < chips[2]
            && chips[2] < w3
            && w3 < markers[3],
        "chain out of order; screen:\n{screen}"
    );
    assert!(
        screen
            .lines()
            .filter(|l| l.contains("Worked for"))
            .all(|l| !l.contains("still running")),
        "markers must never carry a still-running suffix; screen:\n{screen}"
    );

    write_cast_if_requested(&harness, "endline_wakeups_close_with_markers.cast");
}
