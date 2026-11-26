//! Capped tar.gz archive of a session directory for a user-consented
//! `/feedback` trace upload.

/// Size caps for the one-shot feedback archive.
pub(crate) struct ArchiveCaps {
    /// Total packed bytes; packing stops (truncating the archive) once hit.
    pub(crate) archive_bytes: u64,
    /// Per-file bytes; larger files are skipped.
    pub(crate) file_bytes: u64,
}

pub(crate) const FEEDBACK_ARCHIVE_CAPS: ArchiveCaps = ArchiveCaps {
    archive_bytes: 50 * 1024 * 1024,
    file_bytes: 10 * 1024 * 1024,
};

/// Failure modes of the one-shot session-trace archive.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ArchiveError {
    #[error("pack session file: {0}")]
    Pack(#[from] std::io::Error),
    #[error("finalize archive: {0}")]
    Finalize(#[source] std::io::Error),
    #[error("session archive would be empty")]
    Empty,
}

pub(crate) fn build_session_archive(
    session_dir: &std::path::Path,
    session_id: &str,
) -> Result<Vec<u8>, ArchiveError> {
    build_session_archive_with_caps(session_dir, session_id, &FEEDBACK_ARCHIVE_CAPS)
}

fn build_session_archive_with_caps(
    session_dir: &std::path::Path,
    session_id: &str,
    caps: &ArchiveCaps,
) -> Result<Vec<u8>, ArchiveError> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let mut archive_data = Vec::new();
    {
        let encoder = GzEncoder::new(&mut archive_data, Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let packed = add_dir_to_tar(&mut archive, session_dir, session_id, caps)?;
        // Skips (oversized files, races with live writers) can leave nothing
        // packed; an empty gzip helps nobody and must not report `uploaded`.
        if packed == 0 {
            return Err(ArchiveError::Empty);
        }
        archive
            .into_inner()
            .and_then(|encoder| encoder.finish())
            .map_err(ArchiveError::Finalize)?;
    }
    Ok(archive_data)
}

/// Pack `dir` into `archive`, returning how many files were packed.
fn add_dir_to_tar<W: std::io::Write>(
    archive: &mut tar::Builder<W>,
    dir: &std::path::Path,
    prefix: &str,
    caps: &ArchiveCaps,
) -> Result<usize, ArchiveError> {
    use std::path::Component;

    let mut total = 0u64;
    let mut packed = 0usize;
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.path_is_symlink() || entry.file_type().is_dir() || !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(dir) else {
            continue;
        };
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        let room = caps.archive_bytes.saturating_sub(total);
        let limit = caps.file_bytes.min(room);
        if limit == 0 {
            // The cap truncates the archive; what is already packed is still
            // useful for debugging, so stop instead of failing the upload.
            break;
        }
        // Skip-on-error: the session dir has live writers, so entries can be
        // deleted or swapped for symlinks between the lstat and the open.
        let Ok(mut file) = open_regular_nofollow(path) else {
            continue;
        };
        let mut buf = Vec::new();
        let n = std::io::copy(&mut std::io::Read::take(&mut file, limit + 1), &mut buf)?;
        if n > limit {
            continue;
        }
        total = total.saturating_add(n);
        let name = format!("{prefix}/{}", rel.to_string_lossy());
        let mut header = tar::Header::new_gnu();
        header.set_size(n);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, name, buf.as_slice())?;
        packed += 1;
    }
    Ok(packed)
}

/// Open without following symlinks (TOCTOU: a walk entry can be replaced by a
/// symlink after `symlink_metadata`), then re-check the opened fd is a
/// regular file for platforms without `O_NOFOLLOW`.
pub(crate) fn open_regular_nofollow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_names(bytes: &[u8]) -> Vec<String> {
        use flate2::read::GzDecoder;
        let mut archive = tar::Archive::new(GzDecoder::new(bytes));
        archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn session_archive_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("chat_history.jsonl"), b"ok").unwrap();
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, b"do-not-upload").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, dir.path().join("leak")).unwrap();

        let bytes = build_session_archive(dir.path(), "sid").unwrap();
        let names = tar_names(&bytes);
        assert!(
            names.iter().any(|n| n.ends_with("chat_history.jsonl")),
            "{names:?}"
        );
        assert!(
            names.iter().all(|n| !n.ends_with("leak")),
            "symlink must not be packed: {names:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn archive_open_refuses_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"secret").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(open_regular_nofollow(&link).is_err());
        assert!(open_regular_nofollow(&target).is_ok());
    }

    /// Hitting the total cap truncates the archive instead of failing it: a
    /// session just over the cap still uploads what was packed.
    #[test]
    fn archive_truncates_at_total_cap_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jsonl"), vec![b'x'; 8]).unwrap();
        std::fs::write(dir.path().join("b.jsonl"), vec![b'y'; 8]).unwrap();
        std::fs::write(dir.path().join("c.jsonl"), vec![b'z'; 8]).unwrap();

        let caps = ArchiveCaps {
            archive_bytes: 10,
            file_bytes: 10,
        };
        let bytes = build_session_archive_with_caps(dir.path(), "sid", &caps)
            .expect("capped archive must still build");
        let names = tar_names(&bytes);
        assert!(
            !names.is_empty() && names.len() < 3,
            "expected a truncated (but non-empty) archive: {names:?}"
        );
    }

    /// An archive where every file was skipped must fail, not upload an
    /// empty gzip while reporting success.
    #[test]
    fn archive_with_nothing_packed_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("huge.jsonl"), vec![b'x'; 32]).unwrap();

        let caps = ArchiveCaps {
            archive_bytes: 64,
            file_bytes: 8,
        };
        let err = build_session_archive_with_caps(dir.path(), "sid", &caps)
            .expect_err("all-skipped session must not produce an archive");
        assert!(err.to_string().contains("empty"), "{err}");
    }
}
