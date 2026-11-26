//! PTY scroll correctness via the harness (screen state from **echo-agent-pty** /
//! alacritty_terminal).
//!
//! After a large mock response settles (follow-mode pins the viewport to the
//! bottom), scroll **up** then **down** and assert the visible markers move.
//! That path exercises the optimized AllTurns paint window in production.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use echo_agent_pager_pty_harness::{ContentController, PtyHarness, keys, pager_binary};

const ROWS: u16 = 40;
const COLS: u16 = 100;
const LINES: usize = 400;

/// SGR mouse-wheel at (row, col) — 1-indexed terminal coords for CSI.
fn sgr_scroll(button: u16, row: u16, col: u16, count: u16) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..count {
        out.extend_from_slice(format!("\x1b[<{button};{col};{row}M").as_bytes());
    }
    out
}

fn long_markdown(n: usize) -> String {
    let mut out = String::with_capacity(n * 96);
    out.push_str("# Scroll correctness fixture\n\n");
    out.push_str("MARKER_TOP_OF_RESPONSE unique-alpha-token\n\n");
    for i in 0..n {
        out.push_str(&format!("scroll-marker-{i:04} lorem line body text here\n"));
    }
    out.push_str("\nMARKER_BOTTOM_OF_RESPONSE unique-omega-token\n");
    out
}

fn any_marker(s: &str, range: std::ops::Range<usize>) -> bool {
    range
        .clone()
        .any(|i| s.contains(&format!("scroll-marker-{i:04}")))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scroll_up_from_follow_bottom_then_back_down() -> Result<()> {
    let binary = pager_binary().context("resolve pager binary")?;
    let content = ContentController::start()
        .await
        .context("start mock inference server")?;
    content.set_response(long_markdown(LINES));

    let mut harness = PtyHarness::spawn_with_content(&binary, ROWS, COLS, &content, &[])
        .context("spawn pager in PTY")?;

    harness
        .wait_for_text("Quit", Duration::from_secs(20))
        .context("welcome")?;

    harness.inject_keys(b"scroll test\r")?;
    // Follow mode pins the viewport to the bottom, so the top marker only
    // flashes on-screen before scrolling above the viewport — polling for it
    // races a fast stream. Wait for the bottom marker, which stays visible in
    // follow mode once the response reaches it.
    harness
        .wait_for_text("MARKER_BOTTOM_OF_RESPONSE", Duration::from_secs(30))
        .context("response reached bottom while following")?;

    // Wait for stream end. Follow mode pins the viewport to the bottom.
    let settle_deadline = Instant::now() + Duration::from_secs(45);
    loop {
        harness.update(Duration::from_millis(100));
        let s = harness.screen_contents();
        let settled = !s.contains("Responding")
            && (s.contains("MARKER_BOTTOM_OF_RESPONSE") || s.contains("Turn completed"));
        if settled {
            break;
        }
        if Instant::now() > settle_deadline {
            bail!(
                "timed out waiting for response to settle; screen:\n{}",
                harness.screen_contents()
            );
        }
    }
    harness.update(Duration::from_millis(400));

    let at_bottom = harness.screen_contents();
    assert!(
        at_bottom.contains("MARKER_BOTTOM_OF_RESPONSE")
            || any_marker(&at_bottom, 350..400)
            || at_bottom.contains("Turn completed"),
        "follow mode should land near bottom after stream; screen:\n{at_bottom}"
    );
    let bottom_snapshot = at_bottom.clone();

    // PageUp/PageDown plus wheel exercise both scroll directions.
    let wall = Instant::now();
    for _ in 0..30 {
        harness.inject_keys(keys::PGUP)?;
        harness.update(Duration::from_millis(35));
        if !harness.is_running()? {
            bail!("pager exited while PageUp scrolling");
        }
    }
    // Wheel over mid-screen scrollback (1-indexed SGR coords).
    harness.inject_keys(&sgr_scroll(64, 15, 40, 50))?; // 64 = wheel up
    harness.update(Duration::from_millis(200));
    harness.update(Duration::from_millis(300));
    let mid = harness.screen_contents();
    let scroll_up_ms = wall.elapsed().as_millis();

    assert_ne!(
        mid, bottom_snapshot,
        "screen must change after scrolling up; still:\n{mid}"
    );
    assert!(
        !mid.contains("MARKER_BOTTOM_OF_RESPONSE"),
        "bottom marker should leave viewport after scroll-up; screen:\n{mid}"
    );
    // Mid/early markers become visible; not only the last 50 lines.
    assert!(
        any_marker(&mid, 0..340),
        "expected earlier scroll-markers after scroll-up; screen:\n{mid}"
    );

    // Scroll back down toward the end.
    for _ in 0..35 {
        harness.inject_keys(keys::PGDN)?;
        harness.update(Duration::from_millis(35));
        if !harness.is_running()? {
            bail!("pager exited while PageDown scrolling");
        }
    }
    harness.inject_keys(&sgr_scroll(65, 15, 40, 50))?; // 65 = wheel down
    harness.update(Duration::from_millis(200));
    harness.update(Duration::from_millis(400));
    let back_bottom = harness.screen_contents();

    assert!(
        back_bottom.contains("MARKER_BOTTOM_OF_RESPONSE")
            || any_marker(&back_bottom, 340..400)
            || back_bottom.contains("Turn completed"),
        "expected to return near bottom after scroll-down; screen:\n{back_bottom}"
    );
    assert_ne!(
        back_bottom, mid,
        "screen after scroll-down must differ from mid-scroll snapshot"
    );

    eprintln!(
        "echo-agent-pty scroll ok: up_wall_ms={scroll_up_ms} bottom_len={} mid_len={} back_len={}",
        bottom_snapshot.len(),
        mid.len(),
        back_bottom.len()
    );

    let _ = harness.quit();
    Ok(())
}
