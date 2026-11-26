// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;
use echo_agent_pager_pty_harness::{
    InferenceEndpoint, InferenceExpectation, InferenceRequestMatcher,
};

/// Must never appear in the answer text, so the assertions can tell them apart.
const REASONING_SENTINEL: &str = "REASONINGSENTINEL";

/// `crate::glyphs::accent_bar()` on a non-legacy console.
const RAIL: &str = "\u{2503}";

/// Kept alive for the duration of the assertions.
struct Turn {
    harness: PtyHarness,
    _content: ContentController,
    _expectation: InferenceExpectation,
}

/// Drive one minimal turn that streams reasoning and then an answer, under
/// `NO_COLOR=1` — the case that was 100% broken, because the `bg_blend` fade is
/// a complete no-op under the terminal-native palette.
async fn run_reasoning_turn(collapse_thinking: bool) -> Turn {
    // Reasoning summary deltas are a Responses-API stream shape.
    let content = ContentController::start_with_models(vec![
        MockModel::new("test-model").with_api_backend("responses"),
    ])
    .await
    .expect("start content");
    let reasoning = format!("{REASONING_SENTINEL} pondering syllables quietly and at some length");
    let answer = format!("{MOCK_RESPONSE_SENTINEL} the answer body.");
    let expectation = content.expect_response(
        "minimal reasoning-vs-output turn",
        InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
        ScriptedResponse::sse(sse::responses_api_reasoning_and_text_events(
            &reasoning,
            &answer,
            "test-model",
        )),
    );
    content.set_response(answer.clone());

    // Ingestion is gated on this toggle, and the sandbox `$HOME` has no config.
    std::fs::create_dir_all(content.home().join(".echo-agent")).expect("mk .echo-agent");
    std::fs::write(
        content.home().join(".echo-agent/config.toml"),
        "[ui]\nshow_thinking_blocks = true\n",
    )
    .expect("write config");
    if collapse_thinking {
        let echo_agent_home = content.sandbox().echo_agent_home().to_path_buf();
        std::fs::create_dir_all(&echo_agent_home).expect("mk echoagent home");
        std::fs::write(
            echo_agent_home.join("pager.toml"),
            "[terminal]\nminimal_collapse_thinking = true\n",
        )
        .expect("write pager.toml");
    }

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        MINIMAL_ARGS,
        &[("NO_COLOR", "1")],
    )
    .expect("spawn minimal pager");
    harness.set_respond_to_queries(true);

    wait_minimal_ready(&mut harness);
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_full_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn committed");
    harness
        .wait_for_full_text("Thought", Duration::from_secs(10))
        .expect("thinking header committed");
    Turn {
        harness,
        _content: content,
        _expectation: expectation,
    }
}

/// Every styled screen row carrying `needle`, rendered as
/// `["text" dim=… italic=…]` runs: the SGR attributes the terminal emulator
/// actually received, not just the glyphs.
fn styled_rows_with(harness: &PtyHarness, needle: &str) -> Vec<String> {
    harness
        .screen_styled()
        .into_iter()
        .filter(|line| line.runs.iter().any(|r| r.text.contains(needle)))
        .map(|line| {
            line.runs
                .iter()
                .map(|r| {
                    format!(
                        "[{:?} dim={} italic={}]",
                        r.text.trim_end(),
                        r.dim,
                        r.italic
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Reasoning must read as "not the answer" in a static native scrollback, with
/// no blank-row separator, no indent, and under `NO_COLOR` no color delta at
/// all. Three orthogonal cues, on the body rows and off the assistant rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_thinking_is_visually_distinct_from_output() {
    let Turn { mut harness, .. } = run_reasoning_turn(false).await;

    harness
        .wait_for_full_text(REASONING_SENTINEL, Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "reasoning body must be committed: {e}\nfull:\n{}",
                harness.full_text()
            )
        });

    let screen = harness.screen_contents();
    eprintln!("─── minimal screen (NO_COLOR=1) ───\n{screen}\n───");
    for row in styled_rows_with(&harness, REASONING_SENTINEL) {
        eprintln!("reasoning row: {row}");
    }
    for row in styled_rows_with(&harness, MOCK_RESPONSE_SENTINEL) {
        eprintln!("answer    row: {row}");
    }

    // 1. Structural: the rail in column 0.
    let reasoning_row = screen
        .lines()
        .find(|l| l.contains(REASONING_SENTINEL))
        .unwrap_or_else(|| panic!("reasoning row on screen:\n{screen}"));
    assert!(
        reasoning_row.starts_with(RAIL),
        "reasoning must keep its accent rail: {reasoning_row:?}"
    );
    let answer_row = screen
        .lines()
        .find(|l| l.contains(MOCK_RESPONSE_SENTINEL))
        .unwrap_or_else(|| panic!("answer row on screen:\n{screen}"));
    assert!(
        !answer_row.starts_with(RAIL),
        "assistant output must not wear a rail: {answer_row:?}"
    );

    // 2. Attributes: SGR survives NO_COLOR where a foreground blend does not.
    let runs_with = |needle: &str| -> Vec<_> {
        harness
            .screen_styled()
            .into_iter()
            .flat_map(|l| l.runs)
            .filter(|r| r.text.contains(needle))
            .collect::<Vec<_>>()
    };

    let reasoning_runs = runs_with(REASONING_SENTINEL);
    assert!(!reasoning_runs.is_empty(), "no styled reasoning run found");
    for run in &reasoning_runs {
        assert!(run.dim, "reasoning must be dim under NO_COLOR: {run:?}");
        assert!(run.italic, "reasoning must be italic: {run:?}");
    }

    let answer_runs = runs_with(MOCK_RESPONSE_SENTINEL);
    assert!(!answer_runs.is_empty(), "no styled answer run found");
    assert!(
        answer_runs.iter().any(|r| !r.dim && !r.italic),
        "assistant output must stay undimmed and upright: {answer_runs:?}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    quit_minimal(&mut harness);
}

/// The collapsed header advertises the only way back into the body, and
/// `Ctrl+E` must honour the advertisement by re-printing it in full (K10).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_collapse_thinking_toggle_folds_and_ctrl_e_reopens() {
    let Turn { mut harness, .. } = run_reasoning_turn(true).await;

    harness
        .wait_for_full_text("ctrl+e to expand", Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "collapsed reasoning must advertise the expand key: {e}\nfull:\n{}",
                harness.full_text()
            )
        });
    eprintln!(
        "─── collapsed ([terminal] minimal_collapse_thinking = true) ───\n{}\n───",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_full_text(REASONING_SENTINEL),
        "the body must be folded away:\n{}",
        harness.full_text()
    );

    harness.inject_keys(b"\x05").expect("ctrl+e");
    harness
        .wait_for_full_text(REASONING_SENTINEL, Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "ctrl+e must re-print the folded reasoning: {e}\nfull:\n{}",
                harness.full_text()
            )
        });
    eprintln!("─── after ctrl+e ───\n{}\n───", harness.screen_contents());

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    quit_minimal(&mut harness);
}
