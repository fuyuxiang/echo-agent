// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
use echo_agent_pager_pty_harness::StyledLine;

/// Greppable tokens on the two rendered quote rows and the settle sentinel.
const QUOTE_ALPHA: &str = "QUOTE_ALPHA";
const QUOTE_BRAVO: &str = "QUOTE_BRAVO";
const QUOTE_OUTRO: &str = "QUOTE_OUTRO_DONE";

/// Per-cell `(bg, inverse)` styling for one screen row (`StyledLine.line` is
/// 1-based; `row` here is the 0-based `locate_screen_text` row), expanded from
/// runs in column order. Selection highlight shows up as a changed `bg`
/// and/or `inverse` on these cells — same sampling as
/// `stuck_drag_recovers_on_esc_pty`.
fn row_cells(lines: &[StyledLine], row: u16) -> Vec<(Option<String>, bool)> {
    let target = row as usize + 1;
    let mut cells = Vec::new();
    for line in lines {
        if line.line != target {
            continue;
        }
        for run in &line.runs {
            for _ in run.text.chars() {
                cells.push((run.bg.clone(), run.inverse));
            }
        }
    }
    cells
}

/// PTY: drag-select copy across a rendered (pretty-mode) markdown quote block
/// excludes the `│ ` bar prefix from the clipboard — even when the drag starts
/// on the bar cell itself. Exercises the real pager + mouse selection + OSC 52.
///
/// `SSH_CONNECTION` is set deliberately: on macOS the clipboard route only
/// emits OSC 52 when it believes the session is remote (see
/// `resolve_clipboard_route`); the harness strips inherited SSH vars, so this
/// test re-injects a dummy one for OSC 52 readback — same pattern as
/// `recap_header_not_in_selection_pty`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn quote_block_drag_copy_excludes_bars_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!(
        "intro line\n\n> {QUOTE_ALPHA} first\n> {QUOTE_BRAVO} second\n\n{QUOTE_OUTRO} line"
    ));

    let binary = pager_binary().expect("resolve pager binary");
    let overrides: Vec<(String, String)> = vec![(
        "SSH_CONNECTION".into(),
        "scripted-test 1 127.0.0.1 2".into(),
    )];
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut harness = PtyHarness::spawn_with_content_env_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &env_refs,
        Some(content.home()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    // Wait for the last streamed line so the turn is settled before dragging
    // (dragging mid-stream races the selection model rebuild).
    harness
        .wait_for_text(QUOTE_OUTRO, Duration::from_secs(45))
        .unwrap_or_else(|_| {
            panic!(
                "expected settled response with {QUOTE_OUTRO:?}; got:\n{}",
                harness.screen_contents()
            )
        });

    // Sanity: pretty mode is active — the quote bar glyph is on screen.
    assert!(
        harness.contains_text("│"),
        "expected rendered quote bars on screen (pretty mode)\nscreen:\n{}",
        harness.screen_contents()
    );

    // Focus scrollback so the drag targets the message, not the prompt.
    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(10))
        .expect("scrollback focused (Space:prompt hint) after Tab");
    // The settle sentinel streams as part of the SAME turn, so the turn-end
    // relayout (spinner/hint rows disappearing) can land after the wait above
    // and shift the content rows. Give it a generous fixed settle, then
    // locate coordinates from the post-settle screen.
    harness.update(Duration::from_millis(1500));

    let screen = harness.screen_contents();
    let (alpha_row, alpha_col) = locate_screen_text(&screen, QUOTE_ALPHA).unwrap_or_else(|| {
        panic!("could not locate {QUOTE_ALPHA:?}; screen:\n{screen}");
    });
    let (bravo_row, bravo_col) = locate_screen_text(&screen, QUOTE_BRAVO).unwrap_or_else(|| {
        panic!("could not locate {QUOTE_BRAVO:?}; screen:\n{screen}");
    });
    assert_eq!(
        bravo_row,
        alpha_row + 1,
        "quote lines should be consecutive rows; screen:\n{screen}"
    );
    // Precondition: the bar cell really is two columns left of the token.
    let bar_col = alpha_col.saturating_sub(2);
    let alpha_line = screen.lines().nth(alpha_row as usize).unwrap_or("");
    assert_eq!(
        alpha_line.chars().nth(bar_col as usize),
        Some('│'),
        "expected the quote bar at col {bar_col}; line: {alpha_line:?}"
    );

    // Baseline styling of the quote row before any selection exists.
    let base_alpha = row_cells(&harness.screen_styled(), alpha_row);

    // Start the drag ON the bar cell ("│ " sits two columns left of the
    // token) and hold it before releasing, so the live selection overlay can
    // be inspected on the styled screen.
    let bravo_end = bravo_col + "QUOTE_BRAVO second".chars().count() as u16 - 1;
    let mut held = String::new();
    held.push_str(&sgr_mouse(0, alpha_row, bar_col, 'M'));
    held.push_str(&sgr_mouse(32, alpha_row, alpha_col + 4, 'M'));
    held.push_str(&sgr_mouse(32, bravo_row, bravo_end, 'M'));
    harness
        .inject_keys(held.as_bytes())
        .expect("drag across quote block (held)");

    // Poll for the live selection overlay (cheap when healthy, tolerant
    // under CI load), then assert it covers content cells but not the bar.
    let mut during_alpha = Vec::new();
    for _ in 0..20 {
        harness.update(Duration::from_millis(150));
        during_alpha = row_cells(&harness.screen_styled(), alpha_row);
        if during_alpha != base_alpha {
            break;
        }
    }
    assert_ne!(
        during_alpha,
        base_alpha,
        "the held drag should paint a selection highlight on the quote row; screen:\n{}",
        harness.screen_contents()
    );
    // Mid-drag: the bar cell and its following space keep their baseline
    // styling — the highlight starts at the selectable content, proving the
    // prefix is excluded from the selection region.
    for col in [bar_col as usize, bar_col as usize + 1] {
        assert_eq!(
            during_alpha.get(col),
            base_alpha.get(col),
            "bar/prefix cell {col} must not be covered by the selection highlight"
        );
    }
    assert_ne!(
        during_alpha.get(alpha_col as usize),
        base_alpha.get(alpha_col as usize),
        "quote content cells should carry the selection highlight"
    );

    // Release to finish the drag and copy.
    harness
        .inject_keys(sgr_mouse(0, bravo_row, bravo_end, 'm').as_bytes())
        .expect("release drag");

    let payloads = wait_for_osc52_payloads(&mut harness, Duration::from_secs(10));
    assert!(
        !payloads.is_empty(),
        "expected at least one OSC 52 clipboard write; screen:\n{}",
        harness.screen_contents()
    );
    let joined = payloads.join("\n");
    assert!(
        joined.contains(&format!("{QUOTE_ALPHA} first\n{QUOTE_BRAVO} second")),
        "clipboard should contain both quote lines joined by a newline; payloads={payloads:?}"
    );
    assert!(
        !joined.contains('│'),
        "clipboard must not contain the quote bar; payloads={payloads:?}"
    );
    assert!(
        !joined.starts_with(char::is_whitespace),
        "clipboard must not start with the prefix whitespace; payloads={payloads:?}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
