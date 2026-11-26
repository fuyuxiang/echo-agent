//! Reproducible harness for benchmarking clipboard attachment reads
//! (the paste hot path).
//!
//! Runs one `get_attachments()` — the same unified probe the pager's paste
//! pipeline executes — and prints the outcome plus wall time. Benchmark the
//! native in-process read against the `osascript` fallback with hyperfine:
//!
//! ```text
//! # put an image on the pasteboard first, e.g.:
//! #   osascript -e 'set the clipboard to (read (POSIX file "shot.png") as «class PNGf»)'
//! cargo build --release -p echo-agent-shared --example clipboard_probe
//! hyperfine --warmup 2 \
//!   -n native './target/release/examples/clipboard_probe' \
//!   -n osascript 'ECHO_AGENT_CLIPBOARD_NO_NATIVE_READ=1 ./target/release/examples/clipboard_probe'
//! ```
//!
//! Exits non-zero when the read errors so hyperfine aborts loudly instead of
//! averaging failures.

fn main() -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let attachments = echo_agent_shared::clipboard::get_attachments()?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1e3;

    let image = attachments
        .image
        .map(|img| format!("{} ({} bytes)", img.mime_type, img.data.len()));
    let file_urls = attachments
        .file_urls
        .map(|urls| format!("{} path(s)", urls.lines().count()));
    println!(
        "{elapsed_ms:.1} ms  image={}  file_urls={}",
        image.as_deref().unwrap_or("none"),
        file_urls.as_deref().unwrap_or("none"),
    );
    Ok(())
}
