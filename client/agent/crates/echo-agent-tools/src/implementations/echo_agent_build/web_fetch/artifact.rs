use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use fs2::FileExt;

const ARTIFACT_DIR: &str = "web_fetch";
const ALLOCATION_FILE: &str = ".allocation";
const COUNTER_WIDTH: usize = 10;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub(super) struct WebFetchArtifactWriter;

impl WebFetchArtifactWriter {
    pub(super) async fn save(
        &self,
        session_folder: &Path,
        bytes: &[u8],
        extension: &str,
    ) -> anyhow::Result<PathBuf> {
        let dir = session_folder.join(ARTIFACT_DIR);
        tokio::fs::create_dir_all(&dir).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
        }

        let data = bytes.to_vec();
        let extension = extension.to_owned();
        tokio::task::spawn_blocking(move || save_locked(&dir, &data, &extension))
            .await
            .context("spawn_blocking panicked")?
    }
}

fn save_locked(dir: &Path, data: &[u8], extension: &str) -> anyhow::Result<PathBuf> {
    let mut allocation = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(ALLOCATION_FILE))
        .context("open allocation file")?;
    allocation
        .lock_exclusive()
        .context("lock allocation file")?;

    let (max_existing, total_bytes) = scan_artifacts(dir)?;
    let number = reserve_number(&mut allocation, max_existing)?;
    let new_total = total_bytes
        .checked_add(u64::try_from(data.len()).context("artifact size exceeds u64")?)
        .context("artifact byte count overflow")?;
    if new_total > MAX_TOTAL_BYTES {
        anyhow::bail!(
            "byte budget exceeded: {new_total}/{MAX_TOTAL_BYTES} bytes in {ARTIFACT_DIR}"
        );
    }

    let path = dir.join(format!("{number}.{extension}"));
    let mut tmp = tempfile::NamedTempFile::new_in(dir).context("create temp file")?;
    tmp.write_all(data).context("write temp file")?;
    tmp.as_file().sync_all().context("fsync temp file")?;
    tmp.persist_noclobber(&path)
        .with_context(|| format!("persist to {}", path.display()))?;
    Ok(path)
}

fn reserve_number(file: &mut std::fs::File, max_existing: u32) -> anyhow::Result<u32> {
    let stored = read_reserved_number(file)?.unwrap_or(max_existing);
    let next = stored
        .max(max_existing)
        .checked_add(1)
        .context("artifact number exhausted")?;
    file.seek(SeekFrom::Start(0))
        .context("seek allocation file")?;
    write!(file, "{next:0width$}", width = COUNTER_WIDTH).context("write allocation file")?;
    file.set_len(COUNTER_WIDTH as u64)
        .context("truncate allocation file")?;
    file.sync_all().context("fsync allocation file")?;
    Ok(next)
}

fn read_reserved_number(file: &mut std::fs::File) -> anyhow::Result<Option<u32>> {
    let len = file.metadata().context("stat allocation file")?.len();
    if len == 0 {
        return Ok(Some(0));
    }
    if len != COUNTER_WIDTH as u64 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0))
        .context("seek allocation file")?;
    let mut bytes = [0; COUNTER_WIDTH];
    file.read_exact(&mut bytes)
        .context("read allocation file")?;
    Ok(std::str::from_utf8(&bytes)
        .ok()
        .and_then(|stored| stored.parse().ok()))
}

fn scan_artifacts(dir: &Path) -> Result<(u32, u64), std::io::Error> {
    let mut max = 0u32;
    let mut total_bytes = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".tmp") {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        if name.starts_with('.') {
            continue;
        }
        if let Some(stem) = name.split_once('.').map(|(stem, _)| stem)
            && let Ok(number) = stem.parse::<u32>()
        {
            max = max.max(number);
        }
        if let Ok(metadata) = entry.metadata()
            && metadata.is_file()
        {
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| std::io::Error::other("artifact byte count overflow"))?;
        }
    }
    Ok((max, total_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allocation_recovers_malformed_state_and_failed_reservation() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session");
        tokio::fs::create_dir_all(session.join(ARTIFACT_DIR))
            .await
            .unwrap();
        tokio::fs::write(session.join(ARTIFACT_DIR).join("5.txt"), b"existing")
            .await
            .unwrap();
        tokio::fs::write(session.join(ARTIFACT_DIR).join(ALLOCATION_FILE), b"torn")
            .await
            .unwrap();
        let writer = WebFetchArtifactWriter;

        assert!(
            writer
                .save(&session, b"failed", "missing/ext")
                .await
                .is_err()
        );
        let saved = writer.save(&session, b"recovered", "txt").await.unwrap();

        assert_eq!(saved.file_name().unwrap(), "7.txt");
        assert_eq!(
            tokio::fs::read_to_string(session.join(ARTIFACT_DIR).join(ALLOCATION_FILE))
                .await
                .unwrap(),
            "0000000007"
        );
    }

    #[tokio::test]
    async fn concurrent_writes_same_session_have_distinct_intact_files() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = WebFetchArtifactWriter;
        let payload_a: &[u8] = b"payload from writer a";
        let payload_b: &[u8] = b"different payload from writer b";

        let (saved_a, saved_b) = tokio::join!(
            writer.save(tmp.path(), payload_a, "txt"),
            writer.save(tmp.path(), payload_b, "txt"),
        );
        let saved_a = saved_a.unwrap();
        let saved_b = saved_b.unwrap();

        assert_ne!(saved_a, saved_b);
        assert_eq!(tokio::fs::read(saved_a).await.unwrap(), payload_a);
        assert_eq!(tokio::fs::read(saved_b).await.unwrap(), payload_b);
    }
}
