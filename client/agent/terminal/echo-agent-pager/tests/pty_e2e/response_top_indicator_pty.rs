// Per-test-case module for the `pty_e2e` integration test crate.
//
// The ▲ jump-to-response-top indicator: an answer taller than the screen
// leaves the reader at its tail, and the sticky header's gap row offers a
// clickable ▲ that snaps the answer's first line to the top — the mouse
// mirror of the K (previous response) key, and the discoverable fix for
// "echoagent doesn't show you its answer from the top, so you have to scroll a
// lot".
#[allow(unused_imports)]
use super::common::*;

/// First and last line of the streamed answer. 60 bullets over a 50-row
/// terminal guarantees the answer overflows the viewport.
const FIRST_LINE: &str = "ANSWERLINE001";
const LAST_LINE: &str = "ANSWERLINE060";

const UP_INDICATOR: &str = "▲";

/// Unique prompt text so the sticky-header row is unambiguous on screen
/// (the shared [`PROMPT`] "go" collides with hint/footer copy).
const TOP_PROMPT: &str = "SHOWTOPPROMPT";

fn long_answer() -> String {
    (1..=60)
        .map(|i| format!("- ANSWERLINE{i:03} lorem ipsum dolor sit amet consectetur"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Row index of `needle` on screen, for the artifact trace.
fn row_of(screen: &str, needle: &str) -> Option<usize> {
    screen.lines().position(|line| line.contains(needle))
}

/// Artifact trace: the viewport's top rows, where the sticky header, the ▲,
/// and (after the click) the answer's first line all live.
fn dump_top_rows(label: &str, screen: &str) {
    eprintln!("[{label}] top of viewport:");
    for line in screen.lines().take(10) {
        eprintln!("    {}", line.trim_end());
    }
}

/// PTY: stream an answer taller than the terminal, check the ▲ indicator
/// appears under the sticky prompt header, click it, and check the viewport
/// lands on the answer's first line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn response_top_indicator_jumps_to_answer_start() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(long_answer());

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{TOP_PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // Follow mode parks the finished turn at the answer's tail; the ▲
    // renders in the sticky header's gap row as soon as the answer's first
    // line scrolls off the top.
    harness
        .wait_for_text(LAST_LINE, Duration::from_secs(60))
        .expect("answer tail visible (follow mode at the bottom)");
    harness
        .wait_for_text(UP_INDICATOR, Duration::from_secs(10))
        .expect("▲ indicator rendered under the sticky prompt header");

    let screen = harness.screen_contents();
    assert!(
        !screen.contains(FIRST_LINE),
        "the answer must overflow the viewport for this test to prove \
         anything\nscreen:\n{screen}"
    );
    let (arrow_row, arrow_col) =
        locate_screen_text(&screen, UP_INDICATOR).expect("locate ▲ on screen");
    let prompt_row = row_of(&screen, TOP_PROMPT);
    eprintln!(
        "[before click] ▲ at ({arrow_row},{arrow_col}), sticky prompt row {prompt_row:?}, \
         answer tail row {:?}",
        row_of(&screen, LAST_LINE)
    );
    dump_top_rows("before click", &screen);
    // "Under the last user prompt block": the sticky header pins the prompt
    // above the gap row that hosts the ▲.
    let prompt_row = prompt_row.expect("sticky header shows the prompt");
    assert!(
        prompt_row < arrow_row as usize && (arrow_row as usize) < prompt_row + 6,
        "▲ should sit in the gap row just below the pinned prompt\nscreen:\n{screen}"
    );

    // Click the ▲ (SGR left press + release at its cell).
    let click = format!(
        "{}{}",
        sgr_mouse(0, arrow_row, arrow_col, 'M'),
        sgr_mouse(0, arrow_row, arrow_col, 'm')
    );
    harness.inject_keys(click.as_bytes()).expect("click ▲");

    harness
        .wait_for_text(FIRST_LINE, Duration::from_secs(10))
        .expect("answer snaps to its first line after the click");

    let screen = harness.screen_contents();
    let first_row = row_of(&screen, FIRST_LINE);
    eprintln!(
        "[after click] first answer line at row {first_row:?}, last answer line at row {:?}",
        row_of(&screen, LAST_LINE)
    );
    dump_top_rows("after click", &screen);
    assert!(
        !screen.contains(LAST_LINE),
        "after the jump the tail must be off screen again\nscreen:\n{screen}"
    );
    let first_row = first_row.expect("first line is on screen");
    assert!(
        first_row < usize::from(DEFAULT_ROWS) / 3,
        "the answer's first line should be parked near the top of the \
         viewport, found it at row {first_row}\nscreen:\n{screen}"
    );
    assert!(
        !harness.contains_text(UP_INDICATOR),
        "with the answer's top on screen the ▲ must disappear\nscreen:\n{screen}"
    );

    harness.quit().expect("clean quit");
}
