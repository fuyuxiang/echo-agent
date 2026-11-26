//! `large_codeblock` — render a large syntax-highlighted Rust code block
//! and scroll through it.
//!
//! What it stresses: `syntect` highlighting cache, wrapping of long source
//! lines, ScratchBuffer copy for a single oversized entry.

use std::time::{Duration, Instant};

use anyhow::Result;

use super::{BenchResults, ContentController, PtyHarness, wait_for_welcome};
use crate::keys;

const LINES: usize = 400;
const SCROLL_KEYS: usize = 120;
const KEY_INTERVAL: Duration = Duration::from_millis(20);

pub async fn run(harness: &mut PtyHarness, content: &ContentController) -> Result<BenchResults> {
    content.set_response(build_rust_codeblock(LINES));

    wait_for_welcome(harness).await?;
    harness.inject_keys(b"code\r")?;
    harness.wait_for_text("fn code_sample", Duration::from_secs(30))?;
    harness.update(Duration::from_millis(500));
    harness.reset_timing();

    let start = Instant::now();
    for _ in 0..SCROLL_KEYS {
        harness.inject_keys(keys::J)?;
        harness.update(KEY_INTERVAL);
        if !harness.is_running()? {
            break;
        }
    }
    harness.update(Duration::from_millis(250));
    let wall_time = start.elapsed();

    Ok(harness.bench_results("large_codeblock", wall_time))
}

fn build_rust_codeblock(lines: usize) -> String {
    let mut s = String::with_capacity(lines * 60);
    s.push_str("```rust\n");
    for i in 0..lines {
        s.push_str(&format!(
            "fn code_sample_{i}(x: i32) -> i32 {{ x.wrapping_mul({i}).wrapping_add({}) }}\n",
            i + 1
        ));
    }
    s.push_str("```\n");
    s
}
