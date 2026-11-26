// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// **ANSI scrollback content integrity (minimal mode).**
///
/// Regression guard for the `echo-agent-ratatui-inline` rewrite (termwiz →
/// anstyle-parse). Minimal mode commits finalized assistant blocks into the
/// terminal's NATIVE scrollback through `echo_agent_ratatui_inline::Terminal::
/// insert_before` — the production seam of the rewritten crate — so a parsing
/// or offset regression there shows up as truncated, duplicated, or corrupted
/// committed content.
///
/// The response stresses the rendered-ANSI surface: a syntax-highlighted code
/// block (SGR color sequences interleave the text), CJK + emoji wide
/// characters (multi-byte UTF-8, width-2 cells), and enough unique code-block
/// rows (which never markdown-reflow) to overflow the 50-row screen and force
/// the head of the block into scrollback.
///
/// Assertions read scrollback + screen after the commit: head/tail sentinels
/// and every payload row present exactly once (dropped or doubled emissions
/// change the count), wide-char markers intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn ansi_scrollback_content_integrity() {
    const HEAD: &str = "HEADSENTINEL7431";
    const TAIL: &str = "TAILSENTINEL7431";
    const ROWS: usize = 80;

    // Code-block rows render verbatim (no markdown reflow) and syntect
    // highlights the python fence, so committed rows carry SGR sequences.
    let mut response = String::new();
    response.push_str("```python\n");
    response.push_str(&format!("# {HEAD} wide: 你好 WIDEMARK 世界 🚀 EMOJIMARK\n"));
    for i in 0..ROWS {
        response.push_str(&format!(
            "payload_row_{i:02} = \"qzjvxk\"  # comment {i:02}\n"
        ));
    }
    response.push_str(&format!("# {TAIL}\n"));
    response.push_str("```\n");

    let content = ContentController::start().await.expect("start content");
    content.set_response(response);

    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // The block commits to native scrollback when the turn finalizes; poll
    // until the head sentinel (which scrolled far above the live region)
    // lands in scrollback.
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline && !harness.scrollback_text().contains(HEAD) {
        harness.update(Duration::from_millis(100));
    }
    // Precondition: the block head actually committed to NATIVE scrollback.
    // Without this, a timeout above would fall through to `full_text()`
    // (scrollback + screen) assertions, which content that never left the
    // live region could partially satisfy — and fail confusingly.
    assert!(
        harness.scrollback_text().contains(HEAD),
        "committed block head must reach native scrollback\nscrollback:\n{}\nscreen:\n{}",
        harness.scrollback_text(),
        harness.screen_contents(),
    );

    let full = harness.full_text();

    // Head and tail sentinels present exactly once and in order: a duplicated
    // segment emission repeats them, a dropped one loses them.
    for sentinel in [HEAD, TAIL] {
        assert_eq!(
            full.matches(sentinel).count(),
            1,
            "sentinel {sentinel} should appear exactly once\nfull text:\n{full}"
        );
    }
    assert!(
        full.find(HEAD).unwrap() < full.find(TAIL).unwrap(),
        "sentinels out of order\nfull text:\n{full}"
    );

    // Every committed row survives exactly once, in full — an offset bug at a
    // segment boundary truncates or doubles rows.
    for i in 0..ROWS {
        let row = format!("payload_row_{i:02} = \"qzjvxk\"  # comment {i:02}");
        assert_eq!(
            full.matches(&row).count(),
            1,
            "payload row {i:02} should appear exactly once and unmangled\nfull text:\n{full}"
        );
    }

    // Wide chars survive the multi-byte wrap-point handling. Checked as
    // individual chars: the harness's text extraction renders each width-2
    // char followed by its spacer cell ("你 好", not "你好").
    for marker in ["你", "好", "世", "界", "🚀", "WIDEMARK", "EMOJIMARK"] {
        assert!(
            full.contains(marker),
            "wide-char marker {marker:?} missing\nfull text:\n{full}"
        );
    }

    assert!(
        content.has_chat_completion(),
        "mock inference server never received a chat completion\nrequests: {:?}",
        content.requests()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
