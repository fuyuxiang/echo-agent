//! `scroll_stress` — inject rapid `j` keys against a large pre-rendered
//! response, measure frame timing.
//!
//! What it stresses: `render_scrolled_entries_with_scratch`, partial-entry
//! clipping (ScratchBuffer cell copy), `Buffer::diff()`.

use std::time::{Duration, Instant};

use anyhow::Result;

use super::{BenchResults, ContentController, PtyHarness, wait_for_welcome};
use crate::keys;

const LINES: usize = 500;
const SCROLL_KEYS: usize = 200;
const KEY_INTERVAL: Duration = Duration::from_millis(16);

pub async fn run(harness: &mut PtyHarness, content: &ContentController) -> Result<BenchResults> {
    // 1. Prime the mock server with a large markdown response.
    content.set_response(long_markdown_response(LINES));

    // 2. Wait for the pager's splash screen.
    wait_for_welcome(harness).await?;

    // 3. Submit a prompt so the mock inference server returns the big response.
    //    The pager is interactive: type a short prompt then hit Enter.
    harness.inject_keys(b"go\r")?;

    // 4. Wait until the streamed response is on screen.
    //    The response is `Lorem ipsum dolor ...` — look for a word we know
    //    will appear after streaming starts.
    harness.wait_for_text("Lorem", Duration::from_secs(30))?;

    // 5. Let the full response settle, then reset timing to start clean.
    harness.update(Duration::from_millis(500));
    harness.reset_timing();

    // 6. Inject scroll-down keys at a fixed interval while collecting frames.
    let wall_start = Instant::now();
    for _ in 0..SCROLL_KEYS {
        harness.inject_keys(keys::J)?;
        harness.update(KEY_INTERVAL);
        if !harness.is_running()? {
            break;
        }
    }
    // Drain any straggler frames.
    harness.update(Duration::from_millis(250));
    let wall_time = wall_start.elapsed();

    Ok(harness.bench_results("scroll_stress", wall_time))
}

/// Generate `n` lines of predictable markdown. Used both as scenario
/// payload and as a basic smoke test of the wrapping pipeline.
fn long_markdown_response(n: usize) -> String {
    let mut out = String::with_capacity(n * 80);
    out.push_str("# Scroll stress response\n\n");
    for i in 0..n {
        out.push_str(&format!(
            "Line {i}: Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\n"
        ));
    }
    out
}
