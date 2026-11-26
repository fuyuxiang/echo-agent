//! Reconstructs a client-side terminal's log file from its `terminal/output`
//! snapshots, for the truncation `read_file` path and the monitor file tail.
//!
//! TODO: fallback until clients push exact output via an
//! `echo.agent/terminal/output_delta` notification (tracked separately).

use std::path::PathBuf;

pub(crate) struct OutputRecorder {
    path: PathBuf,
    last: String,
    /// Must span the whole client buffer, or a rolled buffer's overlap is missed
    /// and the snapshot is re-appended each poll.
    overlap_window: usize,
    realign_warned: bool,
    file: Option<tokio::fs::File>,
    overlap_s: Vec<u8>,
    overlap_pi: Vec<u32>,
}

impl OutputRecorder {
    pub(crate) fn new(path: PathBuf, output_byte_limit: usize) -> Self {
        Self {
            path,
            last: String::new(),
            overlap_window: output_byte_limit,
            realign_warned: false,
            file: None,
            overlap_s: Vec::new(),
            overlap_pi: Vec::new(),
        }
    }

    pub(crate) fn mirrored(&self) -> &str {
        &self.last
    }

    pub(crate) async fn initialize(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if let Err(e) = tokio::fs::File::create(&self.path).await {
            tracing::debug!(path = %self.path.display(), error = %e, "output recorder: failed to create log file");
        }
    }

    /// Append what `current` adds beyond the previous snapshot, realigning on the
    /// largest overlap once the buffer rolls. On write error `last` is left
    /// unadvanced so the next poll retries, and the error is returned.
    pub(crate) async fn append(&mut self, current: &str) -> std::io::Result<()> {
        // Empty snapshot must not clear the baseline, or the next cumulative one
        // gets re-appended in full.
        if current.is_empty() || current == self.last {
            return Ok(());
        }
        let new_suffix = match current.strip_prefix(self.last.as_str()) {
            Some(suffix) => suffix,
            None => {
                let overlap = largest_overlap(
                    &self.last,
                    current,
                    self.overlap_window,
                    &mut self.overlap_s,
                    &mut self.overlap_pi,
                );
                if overlap == 0 && !self.last.is_empty() && !self.realign_warned {
                    self.realign_warned = true;
                    tracing::warn!(
                        path = %self.path.display(),
                        "output recorder: no overlap between consecutive output snapshots; appending whole snapshot (possible duplication)"
                    );
                }
                &current[overlap..]
            }
        };
        if !new_suffix.is_empty() {
            use tokio::io::AsyncWriteExt;
            if self.file.is_none() {
                self.file = Some(
                    tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&self.path)
                        .await?,
                );
            }
            let write = {
                let file = self.file.as_mut().expect("handle opened above");
                match file.write_all(new_suffix.as_bytes()).await {
                    Ok(()) => file.flush().await,
                    Err(e) => Err(e),
                }
            };
            if let Err(e) = write {
                self.file = None;
                return Err(e);
            }
        }
        self.last.clear();
        self.last.push_str(current);
        Ok(())
    }
}

/// Largest suffix of `last` (within its last `window` bytes) that is a prefix of
/// `current`, via a linear KMP over `current ++ tail`. Best-effort: repetitive
/// output can over-match and drop a segment.
fn largest_overlap(
    last: &str,
    current: &str,
    window: usize,
    s: &mut Vec<u8>,
    pi: &mut Vec<u32>,
) -> usize {
    let cur = current.as_bytes();
    let last_bytes = last.as_bytes();
    if cur.is_empty() || last_bytes.is_empty() {
        return 0;
    }
    let tail = &last_bytes[last_bytes.len().saturating_sub(window)..];

    s.clear();
    s.extend_from_slice(cur);
    s.extend_from_slice(tail);
    pi.clear();
    pi.resize(s.len(), 0);
    let mut k: u32 = 0;
    for i in 1..s.len() {
        while k > 0 && s[i] != s[k as usize] {
            k = pi[(k - 1) as usize];
        }
        if s[i] == s[k as usize] {
            k += 1;
        }
        pi[i] = k;
    }

    let cap = cur.len().min(tail.len());
    let mut overlap = pi[s.len() - 1] as usize;
    while overlap > cap {
        overlap = pi[overlap - 1] as usize;
    }
    while overlap > 0 && !current.is_char_boundary(overlap) {
        overlap -= 1;
    }
    overlap
}

pub(crate) struct LogTail {
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

pub(crate) async fn read_log_tail(path: &std::path::Path, limit: usize) -> Option<LogTail> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let len = file.seek(std::io::SeekFrom::End(0)).await.ok()?;
    let back = len.min(limit as u64);
    let truncated = back < len;
    file.seek(std::io::SeekFrom::End(-i64::try_from(back).ok()?))
        .await
        .ok()?;
    let mut buf = Vec::with_capacity(back as usize);
    file.take(back).read_to_end(&mut buf).await.ok()?;
    let head = buf
        .iter()
        .position(|&b| b & 0xC0 != 0x80)
        .unwrap_or(buf.len());
    let text = match std::str::from_utf8(&buf[head..]) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&buf[head..head + e.valid_up_to()])
            .expect("valid_up_to() yields a valid UTF-8 prefix"),
    };
    if text.is_empty() {
        return None;
    }
    Some(LogTail {
        text: text.to_owned(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ov(last: &str, current: &str, window: usize) -> usize {
        let mut s = Vec::new();
        let mut pi = Vec::new();
        largest_overlap(last, current, window, &mut s, &mut pi)
    }

    #[test]
    fn largest_overlap_finds_rolling_tail_alignment() {
        assert_eq!(ov("line1\nline2\n", "ne2\nline3\n", 8192), "ne2\n".len());
        assert_eq!(ov("aaaa", "bbbb", 8192), 0);
        assert_eq!(ov("abc", "abc", 8192), 3);
        assert_eq!(ov("xxabcdef", "abcdefyy", 3), 0);
        assert_eq!(ov("", "abc", 8192), 0);
        assert_eq!(ov("abc", "", 8192), 0);
        assert_eq!(ov("xé", "é!", 8192), "é".len());
        assert_eq!(ov("abababab", "ababXY", 8192), 4);
        assert_eq!(ov("xxabcxx", "abc", 8192), 0);
    }

    #[tokio::test]
    async fn recorder_appends_cumulative_suffixes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("task.log");
        let mut recorder = OutputRecorder::new(path.clone(), 1024 * 1024);
        recorder.initialize().await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");

        recorder.append("line1\n").await.unwrap();
        recorder.append("line1\nline2\n").await.unwrap();
        recorder.append("line1\nline2\n").await.unwrap();
        recorder.append("line1\nline2\nline3\n").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line1\nline2\nline3\n"
        );
    }

    #[tokio::test]
    async fn recorder_retries_the_suffix_after_a_failed_write() {
        let dir = tempfile::tempdir().unwrap();

        let mut recorder = OutputRecorder::new(dir.path().to_path_buf(), 1024 * 1024);
        assert!(recorder.append("line1\n").await.is_err());
        assert_eq!(recorder.last, "");

        let path = dir.path().join("task.log");
        recorder.path = path.clone();
        recorder.append("line1\nline2\n").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "line1\nline2\n");
    }

    #[tokio::test]
    async fn recorder_reconstructs_stream_across_repeated_rolls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task.log");
        let limit = 8usize;
        let mut recorder = OutputRecorder::new(path.clone(), limit);
        recorder.initialize().await;

        let full = "abcdefghijklmnopqrstuvwxyz";
        for end in 1..=full.len() {
            let start = end.saturating_sub(limit);
            recorder.append(&full[start..end]).await.unwrap();
        }

        assert_eq!(std::fs::read_to_string(&path).unwrap(), full);
    }

    #[tokio::test]
    async fn recorder_ignores_empty_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task.log");
        let mut recorder = OutputRecorder::new(path.clone(), 1024 * 1024);
        recorder.initialize().await;

        recorder.append("line1\nline2\n").await.unwrap();
        recorder.append("").await.unwrap();
        recorder.append("line1\nline2\nline3\n").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line1\nline2\nline3\n"
        );
    }

    #[tokio::test]
    async fn read_log_tail_drops_leading_partial_char() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lead.log");
        // "€ab" is [E2 82 AC 61 62]; a 3-byte limit cuts inside the euro sign.
        tokio::fs::write(&path, "€ab").await.unwrap();
        let tail = read_log_tail(&path, 3).await.unwrap();
        assert_eq!(tail.text, "ab");
        assert!(tail.truncated);
    }

    #[tokio::test]
    async fn read_log_tail_drops_trailing_partial_char() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.log");
        // File ends mid-character: "ab" then the first two bytes of the euro sign.
        tokio::fs::write(&path, [b'a', b'b', 0xE2, 0x82])
            .await
            .unwrap();
        let tail = read_log_tail(&path, 1024).await.unwrap();
        assert_eq!(tail.text, "ab");
        assert!(!tail.truncated);
    }
}
