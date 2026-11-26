// Per-test-case module for the `pty_e2e` integration test crate.
//
// Regression pin: Tab inside the `ask_user_question` card used to hand focus
// to the scrollback while the card stayed drawn and the shortcuts bar kept
// advertising card keys.
#[allow(unused_imports)]
use super::common::*;

const FIRST_QUESTION: &str = "What kind of work are you most interested in right now?";
const SECOND_QUESTION: &str = "How deep should the answer go?";

/// Answer rows of each question in render order.
const FIRST_ROWS: [&str; 4] = [
    "Writing or editing code",
    "Explaining the codebase",
    "Debugging a failure",
    "Type your answer here",
];
const SECOND_ROWS: [&str; 3] = ["A quick summary", "Every detail", "Type your answer here"];

const DONE_SENTINEL: &str = "QUESTIONTABDONE";

const TAB: &[u8] = b"\t";
/// `BackTab` — the xterm encoding of Shift+Tab.
const SHIFT_TAB: &[u8] = b"\x1b[Z";
const RIGHT: &[u8] = b"\x1b[C";
const ESC: &[u8] = b"\x1b";

const FOCUSED_HINT: &str = "Tab:next answer";
const PARKED_HINT: &str = "Tab/Space:question";

fn ask_user_question_args() -> String {
    let option =
        |label: &str, description: &str| json!({ "label": label, "description": description });
    json!({
        "questions": [
            {
                "question": FIRST_QUESTION,
                "options": [
                    option(FIRST_ROWS[0], "Implement features or fix bugs"),
                    option(FIRST_ROWS[1], "Understand how things work"),
                    option(FIRST_ROWS[2], "Track down a flake"),
                ],
            },
            {
                "question": SECOND_QUESTION,
                "options": [
                    option(SECOND_ROWS[0], "Just the headline"),
                    option(SECOND_ROWS[1], "Walk me through it"),
                ],
            },
        ]
    })
    .to_string()
}

/// Text of the answer row that currently carries the cursor band.
///
/// Reading the styled screen is the only way to observe cursor position from
/// outside the process.
fn cursor_row(harness: &PtyHarness) -> Option<String> {
    let labels: Vec<&str> = FIRST_ROWS
        .iter()
        .chain(SECOND_ROWS.iter())
        .copied()
        .collect();
    let mut rows: Vec<(String, String)> = Vec::new();
    for line in harness.screen_styled() {
        let text: String = line.runs.iter().map(|r| r.text.as_str()).collect();
        let Some(label) = labels.iter().find(|label| text.contains(**label)) else {
            continue;
        };
        let bg = line
            .runs
            .iter()
            .find(|run| run.text.contains(*label))
            .and_then(|run| run.bg.clone())
            .unwrap_or_default();
        rows.push(((*label).to_string(), bg));
    }
    rows.iter()
        .find(|(_, bg)| rows.iter().filter(|(_, other)| other == bg).count() == 1)
        .map(|(label, _)| label.clone())
}

fn expect_cursor_row(harness: &mut PtyHarness, expected: &str, step: &str) {
    let outcome = harness.wait_until(
        &format!("{step}: cursor on {expected:?}"),
        CURSOR_TIMEOUT,
        |h| cursor_row(h).as_deref() == Some(expected),
    );
    eprintln!("[cursor] {step}: {:?}", cursor_row(harness));
    outcome.unwrap_or_else(|e| panic!("{e}"));
}

fn expect_text(harness: &mut PtyHarness, needle: &str, step: &str) {
    eprintln!("[screen] {step}: waiting for {needle:?}");
    harness
        .wait_for_text(needle, CURSOR_TIMEOUT)
        .unwrap_or_else(|e| panic!("{step}: {e}"));
}

const CURSOR_TIMEOUT: Duration = Duration::from_secs(10);

/// Every string leaf of a recorded request body.
fn string_leaves(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|item| string_leaves(item, out)),
        serde_json::Value::Object(fields) => {
            fields.values().for_each(|field| string_leaves(field, out))
        }
        _ => {}
    }
}

/// Drive the full Tab / Esc / answer-walk contract on a live `ask_user_question`
/// card. `vim_mode` only changes the seed config — the contract is the same.
async fn assert_question_tab_contract(vim_mode: bool) {
    let content = ContentController::start().await.expect("start content");
    if vim_mode {
        seed_ui_config(&content, "vim_mode = true\nsimple_mode = false");
    }
    let call_id = if vim_mode {
        "call_ask_tab_vim"
    } else {
        "call_ask_tab"
    };
    let _turn = expect_tool_turn(
        &content,
        call_id,
        "ask_user_question",
        ask_user_question_args(),
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
    .expect("spawn pager with content");

    let dump = |suffix: &str| {
        if vim_mode {
            format!("question_tab_vim_{suffix}")
        } else {
            format!("question_tab_{suffix}")
        }
    };

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(FIRST_ROWS[2], Duration::from_secs(30))
        .expect("question card renders");
    expect_cursor_row(
        &mut harness,
        FIRST_ROWS[0],
        "card opens on the first answer",
    );
    write_screen_dump_if_requested(&harness, &dump("00_card_open"));

    harness.inject_keys(ESC).expect("Esc parks the card");
    harness
        .wait_for_text(PARKED_HINT, Duration::from_secs(10))
        .expect("the bar names the way back into the parked card");
    assert!(
        !harness.contains_text(FOCUSED_HINT),
        "a parked card must not keep advertising keys it will not receive\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.contains_text(FIRST_ROWS[2]),
        "the card stays drawn and answerable while parked\nscreen:\n{}",
        harness.screen_contents()
    );
    write_screen_dump_if_requested(&harness, &dump("00b_parked"));

    // Vim mode only: parked j/k are scrollback nav and must not walk the card
    // behind the pane. (Without vim mode a bare letter focuses the prompt and
    // types — different path, not part of this contract.)
    if vim_mode {
        let parked_cursor = cursor_row(&harness);
        harness.inject_keys(b"j").expect("parked j");
        harness.inject_keys(b"k").expect("parked k");
        assert_eq!(
            cursor_row(&harness),
            parked_cursor,
            "parked j/k must not move the answer cursor\nscreen:\n{}",
            harness.screen_contents()
        );
        assert!(
            harness.contains_text(PARKED_HINT),
            "scrollback keeps the keyboard after parked j/k\nscreen:\n{}",
            harness.screen_contents()
        );
    }

    harness.inject_keys(TAB).expect("Tab back into the card");
    harness
        .wait_for_text(FOCUSED_HINT, Duration::from_secs(10))
        .expect("Tab hands the keyboard back");
    expect_cursor_row(
        &mut harness,
        FIRST_ROWS[0],
        "the walk resumes where it was parked",
    );

    // Focused j walks answers the same way as Tab — vim mode must not steal it.
    harness.inject_keys(b"j").expect("focused j");
    expect_cursor_row(
        &mut harness,
        FIRST_ROWS[1],
        "focused j walks the next answer",
    );
    harness.inject_keys(b"k").expect("focused k");
    expect_cursor_row(&mut harness, FIRST_ROWS[0], "focused k walks back");

    harness.inject_keys(b" ").expect("Space marks the answer");

    for (step, expected) in FIRST_ROWS.iter().enumerate().skip(1) {
        harness.inject_keys(TAB).expect("Tab");
        expect_cursor_row(&mut harness, expected, &format!("Tab #{step}"));
    }
    write_screen_dump_if_requested(&harness, &dump("01_first_question_walked"));

    harness.inject_keys(TAB).expect("Tab off the last answer");
    expect_cursor_row(
        &mut harness,
        FIRST_ROWS[0],
        "Tab past the last answer wraps to the first",
    );
    expect_text(
        &mut harness,
        "[1/2]",
        "the walk never leaves the question on screen",
    );
    write_screen_dump_if_requested(&harness, &dump("02_wrapped_to_first"));
    assert!(
        harness.contains_text(FOCUSED_HINT),
        "the card still owns the keyboard after the wrap\nscreen:\n{}",
        harness.screen_contents()
    );

    harness
        .inject_keys(SHIFT_TAB)
        .expect("Shift+Tab wraps back");
    expect_cursor_row(
        &mut harness,
        FIRST_ROWS[3],
        "before the first answer, Shift+Tab wraps to the last",
    );
    expect_text(&mut harness, "[1/2]", "and still inside question 1");
    write_screen_dump_if_requested(&harness, &dump("03_wrapped_to_last"));

    harness.inject_keys(RIGHT).expect("→ to question 2");
    expect_cursor_row(&mut harness, SECOND_ROWS[0], "→ crosses into question 2");
    expect_text(&mut harness, "[2/2]", "→ moves between questions, not Tab");
    write_screen_dump_if_requested(&harness, &dump("04_second_question"));

    harness.inject_keys(TAB).expect("Tab");
    expect_cursor_row(
        &mut harness,
        SECOND_ROWS[1],
        "the walk resumes inside question 2",
    );
    harness.inject_keys(b"\r").expect("submit answers");
    harness
        .wait_for_text(DONE_SENTINEL, Duration::from_secs(30))
        .expect("agent turn resumes after the answers");
    write_screen_dump_if_requested(&harness, &dump("99_submitted"));

    let mut leaves = Vec::new();
    for body in content.request_bodies() {
        string_leaves(&body, &mut leaves);
    }
    let answered: Vec<&String> = leaves
        .iter()
        .filter(|leaf| leaf.contains("has answered your questions"))
        .collect();
    eprintln!("[tool result vim_mode={vim_mode}] {answered:?}");
    for expected in [
        format!("\"{FIRST_QUESTION}\"=\"{}\"", FIRST_ROWS[0]),
        format!("\"{SECOND_QUESTION}\"=\"{}\"", SECOND_ROWS[1]),
    ] {
        assert!(
            answered.iter().any(|leaf| leaf.contains(&expected)),
            "tool result should carry {expected:?}, got {answered:?}"
        );
    }

    harness.quit().expect("clean quit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn question_tab_cycles_answers() {
    assert_question_tab_contract(false).await;
}

/// Same contract under `[ui].vim_mode = true`: focused j/k walk answers, Esc
/// parks, parked j/k stay on the scrollback, Tab returns, wrap and submit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn question_tab_cycles_answers_in_vim_mode() {
    assert_question_tab_contract(true).await;
}
