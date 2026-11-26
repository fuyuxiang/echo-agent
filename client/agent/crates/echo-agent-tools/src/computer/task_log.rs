//! Reads part of a task's log file without loading all of it.

use std::path::Path;

use tokio::io::AsyncReadExt;

/// Far more than any tool shows the model, so this bounds memory only. The
/// assertion pins the built-in budget; a runtime budget raised past this
/// would see the prefix instead of the whole log.
pub(crate) const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;
const _: () = assert!(MAX_SNAPSHOT_BYTES > crate::DEFAULT_TOOL_OUTPUT_BYTES);

/// Reads up to `max_bytes` from the start of `path`, and whether the file
/// continues past it. An unreadable file reads as empty and incomplete.
pub(crate) async fn read_prefix(path: &Path, max_bytes: usize) -> (String, bool) {
    match read_bounded(path, max_bytes).await {
        Ok((bytes, more)) => {
            let (text, cut) = decode(&bytes);
            (text, more || cut)
        }
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "task log could not be read");
            (String::new(), true)
        }
    }
}

async fn read_bounded(path: &Path, max_bytes: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let file = tokio::fs::File::open(path).await?;
    let mut buf = Vec::new();
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut buf)
        .await?;
    let more = buf.len() > max_bytes;
    buf.truncate(max_bytes);
    Ok((buf, more))
}

/// The flag reports a trailing split character that was dropped, so a cut
/// cannot read as the whole log.
fn decode(buf: &[u8]) -> (String, bool) {
    match std::str::from_utf8(buf) {
        Ok(text) => (text.to_owned(), false),
        Err(error) if error.error_len().is_none() => (
            String::from_utf8_lossy(&buf[..error.valid_up_to()]).into_owned(),
            true,
        ),
        Err(_) => (String::from_utf8_lossy(buf).into_owned(), false),
    }
}

#[cfg(test)]
#[path = "task_log_tests.rs"]
mod tests;
