//! Session-scoped file storage with crash-safe atomic writes and budgets.
//!
//! Each [`SessionFileWriter`] manages a single subdirectory and file
//! extension, producing files named `1.jpg`, `2.mp4`, `3.pdf`, etc.
//! The counter is lazily initialised from existing files on disk so
//! resumed sessions don't overwrite previous output.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use anyhow::Context;

const IMAGE_MAX_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB
const VIDEO_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GB
const DEFAULT_MAX_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB

fn budget_for(dir_name: &str) -> u64 {
    match dir_name {
        "images" => IMAGE_MAX_BYTES,
        "videos" => VIDEO_MAX_BYTES,
        _ => DEFAULT_MAX_BYTES,
    }
}

/// Persists numbered files to `<session_folder>/<dir_name>/<N>.<ext>`.
///
/// Writes are crash-safe: data is written to a temp file, fsynced, then
/// atomically renamed into place via `tempfile::NamedTempFile::persist`.
#[derive(Clone, Debug)]
pub(crate) struct SessionFileWriter {
    dir_name: &'static str,
    ext: &'static str,
    counter: Arc<AtomicU32>,
    bytes_written: Arc<AtomicU64>,
    max_total_bytes: u64,
}

impl SessionFileWriter {
    pub(crate) fn new(dir_name: &'static str, ext: &'static str) -> Self {
        let max_total_bytes = budget_for(dir_name);
        Self {
            dir_name,
            ext,
            counter: Arc::new(AtomicU32::new(0)),
            bytes_written: Arc::new(AtomicU64::new(0)),
            max_total_bytes,
        }
    }

    /// Save `bytes` to the next numbered file, returning the absolute path.
    ///
    /// `ext_override` writes a different file type without needing a
    /// separate writer (e.g. saving a PNG from a JPG-default writer).
    #[tracing::instrument(skip_all, fields(dir = self.dir_name))]
    pub(crate) async fn save(
        &self,
        session_folder: &Path,
        bytes: &[u8],
        ext_override: Option<&str>,
    ) -> anyhow::Result<PathBuf> {
        let dir = session_folder.join(self.dir_name);

        if !dir.exists() {
            tokio::fs::create_dir_all(&dir).await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
            }
        }

        // Lazy-init: scan existing files for counter resume + byte accounting.
        // compare_exchange ensures only the winner initialises; losers no-op.
        if self.counter.load(Ordering::Relaxed) == 0
            && let Ok((max_n, total_bytes)) = scan_dir_stats(&dir).await
            && self
                .counter
                .compare_exchange(0, max_n, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.bytes_written.fetch_add(total_bytes, Ordering::Relaxed);
        }

        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;

        let size = bytes.len() as u64;
        let new_total = self.bytes_written.fetch_add(size, Ordering::Relaxed) + size;
        if new_total > self.max_total_bytes {
            self.bytes_written.fetch_sub(size, Ordering::Relaxed);
            self.counter.fetch_sub(1, Ordering::Relaxed);
            anyhow::bail!(
                "byte budget exceeded: {}/{} bytes in {}",
                new_total,
                self.max_total_bytes,
                self.dir_name
            );
        }

        let ext = ext_override.unwrap_or(self.ext);
        let path = dir.join(format!("{n}.{ext}"));

        // Atomic write: tempfile -> sync_all -> persist, all in one
        // spawn_blocking call to avoid blocking the async executor.
        let dest = path.clone();
        let target_dir = dir;
        let data = bytes.to_vec();
        let result: anyhow::Result<()> = tokio::task::spawn_blocking(move || {
            let tmp = tempfile::NamedTempFile::new_in(&target_dir).context("create temp file")?;
            std::fs::write(tmp.path(), &data).context("write temp file")?;
            tmp.as_file().sync_all().context("fsync temp file")?;
            tmp.persist(&dest)
                .with_context(|| format!("persist to {}", dest.display()))?;
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?;

        if let Err(e) = result {
            self.counter.fetch_sub(1, Ordering::Relaxed);
            self.bytes_written.fetch_sub(size, Ordering::Relaxed);
            return Err(e);
        }

        tracing::debug!(bytes = size, "file saved");
        Ok(path)
    }
}

/// Scan `dir` for stats: (max counter N from any `N.*` file, total bytes of all files).
/// Also removes orphan `.tmp*` files left behind by interrupted writes.
async fn scan_dir_stats(dir: &Path) -> Result<(u32, u64), std::io::Error> {
    let mut max = 0u32;
    let mut total_bytes = 0u64;
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Remove orphan temp files from interrupted writes
        if name_str.starts_with(".tmp") {
            let _ = tokio::fs::remove_file(entry.path()).await;
            continue;
        }

        // Track the highest numbered file (any extension)
        if let Some(stem) = name_str.split_once('.').map(|(s, _)| s)
            && let Ok(n) = stem.parse::<u32>()
        {
            max = max.max(n);
        }

        // Sum bytes for budget init
        if let Ok(meta) = entry.metadata().await {
            total_bytes += meta.len();
        }
    }
    Ok((max, total_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn save_creates_numbered_files() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = SessionFileWriter::new("images", "jpg");

        let p1 = writer.save(tmp.path(), b"img1", None).await.unwrap();
        let p2 = writer.save(tmp.path(), b"img2", None).await.unwrap();

        assert_eq!(p1.file_name().unwrap(), "1.jpg");
        assert_eq!(p2.file_name().unwrap(), "2.jpg");
        assert_eq!(tokio::fs::read(&p1).await.unwrap(), b"img1");
        assert_eq!(tokio::fs::read(&p2).await.unwrap(), b"img2");
    }

    #[tokio::test]
    async fn save_resumes_counter_from_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("downloads");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("1.pdf"), b"").await.unwrap();
        tokio::fs::write(dir.join("5.pdf"), b"").await.unwrap();
        tokio::fs::write(dir.join("readme.txt"), b"").await.unwrap();

        let writer = SessionFileWriter::new("downloads", "pdf");
        let path = writer.save(tmp.path(), b"new", None).await.unwrap();

        assert_eq!(path.file_name().unwrap(), "6.pdf");
    }

    #[tokio::test]
    async fn save_uses_ext_override() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = SessionFileWriter::new("downloads", "pdf");

        let p1 = writer.save(tmp.path(), b"doc", Some("docx")).await.unwrap();
        assert_eq!(p1.file_name().unwrap(), "1.docx");
    }

    #[tokio::test]
    async fn scan_dir_stats_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let (max, bytes) = scan_dir_stats(tmp.path()).await.unwrap();
        assert_eq!(max, 0);
        assert_eq!(bytes, 0);
    }

    #[tokio::test]
    async fn scan_dir_stats_finds_max_and_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("1.mp4"), b"aaaa")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("3.mp4"), b"bbbbbb")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("2.mp4"), b"cc")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("hero.mp4"), b"")
            .await
            .unwrap();
        let (max, bytes) = scan_dir_stats(tmp.path()).await.unwrap();
        assert_eq!(max, 3);
        assert_eq!(bytes, 4 + 6 + 2); // aaaa + bbbbbb + cc
    }

    #[tokio::test]
    async fn scan_dir_stats_cleans_orphan_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join(".tmp123"), b"orphan")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("1.jpg"), b"keep")
            .await
            .unwrap();
        let (max, _) = scan_dir_stats(tmp.path()).await.unwrap();
        assert_eq!(max, 1);
        assert!(!tmp.path().join(".tmp123").exists());
    }

    #[tokio::test]
    async fn scan_dir_stats_missing_dir() {
        let dir = Path::new("/nonexistent/path/that/does/not/exist");
        assert!(scan_dir_stats(dir).await.is_err());
    }
}
