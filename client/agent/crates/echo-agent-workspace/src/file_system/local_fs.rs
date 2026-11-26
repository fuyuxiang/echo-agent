use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::fs;

use crate::file_system::{AsyncFileSystem, FsError};

pub struct LocalFs {
    root: PathBuf,
    remount: OnceLock<PathBuf>,
}

impl LocalFs {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            remount: OnceLock::new(),
        }
    }
}

#[async_trait::async_trait]
impl AsyncFileSystem for LocalFs {
    fn root(&self) -> &Path {
        self.remount.get().unwrap_or(&self.root)
    }

    fn remount_root(&self, root: PathBuf) {
        let _ = self.remount.set(root);
    }

    async fn exists(&self, path: &Path) -> Result<bool, FsError> {
        Ok(fs::try_exists(path).await?)
    }

    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        Ok(fs::read(path).await?)
    }

    async fn try_read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, FsError> {
        match fs::read(path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), FsError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).await?;
        }
        fs::write(path, data).await?;
        Ok(())
    }

    async fn delete_file(&self, path: &Path) -> Result<(), FsError> {
        fs::remove_file(path).await?;
        Ok(())
    }
}

const MAX_REWIND_FILE_BYTES: usize = 16 * 1024 * 1024;

fn rewind_file_path(root: &Path, relative: &Path) -> io::Result<PathBuf> {
    let mut target = root.canonicalize()?;
    let mut parts = relative.components().peekable();
    if parts.peek().is_none() {
        return Err(io::Error::other("empty rewind path"));
    }
    while let Some(part) = parts.next() {
        let std::path::Component::Normal(part) = part else {
            return Err(io::Error::other(
                "rewind path must stay inside the workspace",
            ));
        };
        target.push(part);
        match std::fs::symlink_metadata(&target) {
            Ok(meta)
                if parts.peek().is_some() && meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(meta) if parts.peek().is_none() && meta.is_file() => {}
            Err(error) if parts.peek().is_none() && error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(io::Error::other("rewind path is not a regular file")),
        }
    }
    Ok(target)
}

/// Read the text version of a local workspace file for a protected rewind.
/// An inaccessible, binary, or unsafe path is always distinct from an absent file.
pub fn read_rewind_file(root: &Path, relative: &Path) -> io::Result<Option<String>> {
    let target = rewind_file_path(root, relative)?;
    read_rewind_path(&target)
}

fn read_rewind_path(target: &Path) -> io::Result<Option<String>> {
    match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err(io::Error::other("rewind path is not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(target) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > MAX_REWIND_FILE_BYTES as u64 {
        return Err(io::Error::other("unsupported rewind file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_REWIND_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_REWIND_FILE_BYTES {
        return Err(io::Error::other("rewind file is too large"));
    }
    String::from_utf8(bytes).map(Some).map_err(io::Error::other)
}

#[derive(Debug, PartialEq, Eq)]
pub enum RewindFileRestore {
    Restored { preserved_edit: Option<PathBuf> },
    Conflict(Option<PathBuf>),
}

/// Restore a reviewed version without overwriting a competing save. The
/// displaced file stays on disk, including the inode an editor may still
/// have open. Prefer the private session directory for these safety copies.
pub fn restore_rewind_file(
    root: &Path,
    relative: &Path,
    expected: Option<&str>,
    desired: Option<&str>,
    archive: &Path,
) -> io::Result<RewindFileRestore> {
    let target = rewind_file_path(root, relative)?;
    if read_rewind_path(&target)?.as_deref() != expected {
        return Ok(RewindFileRestore::Conflict(None));
    }
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("missing rewind parent"))?;
    let mut staged = desired
        .map(|content| {
            if content.len() > MAX_REWIND_FILE_BYTES {
                return Err(io::Error::other("rewind file is too large"));
            }
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(content.as_bytes())?;
            file.as_file().sync_all()?;
            Ok(file)
        })
        .transpose()?;

    if rewind_file_path(root, relative)? != target {
        return Ok(RewindFileRestore::Conflict(None));
    }
    let Some(expected) = expected else {
        return match staged
            .take()
            .map(|file| file.persist_noclobber(&target))
            .transpose()
        {
            Ok(_) => Ok(RewindFileRestore::Restored {
                preserved_edit: None,
            }),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                Ok(RewindFileRestore::Conflict(None))
            }
            Err(error) => Err(error.error),
        };
    };
    let captured = match claim_rewind_file(&target, archive) {
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            claim_rewind_file(&target, parent)
        }
        result => result,
    }?;
    let Some(captured) = captured else {
        return Ok(RewindFileRestore::Conflict(None));
    };
    finish_rewind_claim(root, relative, &target, captured, expected, staged)
}

fn claim_rewind_file(target: &Path, archive: &Path) -> io::Result<Option<PathBuf>> {
    let staging = tempfile::Builder::new()
        .prefix(".echoagent-rewind-")
        .tempdir_in(archive)?;
    let captured = staging.path().join(
        target
            .file_name()
            .ok_or_else(|| io::Error::other("missing rewind file name"))?,
    );
    match std::fs::rename(target, &captured) {
        Ok(()) => {
            // Never let a temporary-directory destructor discard the original.
            let _ = staging.keep();
            #[cfg(unix)]
            for dir in [
                archive,
                captured.parent().unwrap(),
                target.parent().unwrap(),
            ] {
                if let Err(error) = std::fs::File::open(dir).and_then(|dir| dir.sync_all()) {
                    tracing::warn!(%error, path = %dir.display(), "could not sync rewind safety copy directory");
                }
            }
            Ok(Some(captured))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn finish_rewind_claim(
    root: &Path,
    relative: &Path,
    target: &Path,
    captured: PathBuf,
    expected: &str,
    staged: Option<tempfile::NamedTempFile>,
) -> io::Result<RewindFileRestore> {
    let same_path = || rewind_file_path(root, relative).is_ok_and(|path| path == target);
    let conflict = || {
        // Put the displaced version back only if nobody has occupied the
        // name since the claim. The safety copy survives every failure.
        let restored = same_path()
            && std::fs::symlink_metadata(&captured).is_ok_and(|meta| meta.is_file())
            && std::fs::hard_link(&captured, target).is_ok();
        if restored {
            let _ = std::fs::remove_file(&captured);
            if let Some(parent) = captured.parent() {
                let _ = std::fs::remove_dir(parent);
            }
            RewindFileRestore::Conflict(None)
        } else {
            RewindFileRestore::Conflict(Some(captured.clone()))
        }
    };
    let permissions = match std::fs::symlink_metadata(&captured) {
        Ok(meta) if meta.is_file() && !meta.permissions().readonly() => meta.permissions(),
        _ => return Ok(conflict()),
    };
    if !same_path()
        || !read_rewind_path(&captured).is_ok_and(|text| text.as_deref() == Some(expected))
    {
        return Ok(conflict());
    }
    if let Some(file) = staged {
        if file.as_file().set_permissions(permissions).is_err()
            || file.as_file().sync_all().is_err()
            || !same_path()
            || file.persist_noclobber(target).is_err()
        {
            return Ok(conflict());
        }
    } else if !std::fs::symlink_metadata(target)
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
    {
        return Ok(conflict());
    }
    let preserved_edit = (!read_rewind_path(&captured)
        .is_ok_and(|text| text.as_deref() == Some(expected)))
    .then_some(captured);
    Ok(RewindFileRestore::Restored { preserved_edit })
}

#[cfg(test)]
mod rewind_tests {
    use super::*;

    #[test]
    fn checks_the_claimed_version_when_an_editor_saved_after_preview() {
        let root = tempfile::tempdir().unwrap();
        let archive = tempfile::tempdir_in(root.path()).unwrap();
        let relative = Path::new("note.txt");
        let target = rewind_file_path(root.path(), relative).unwrap();
        std::fs::write(&target, "new editor content").unwrap();
        let captured = claim_rewind_file(&target, archive.path()).unwrap().unwrap();
        assert_eq!(
            finish_rewind_claim(root.path(), relative, &target, captured, "reviewed", None)
                .unwrap(),
            RewindFileRestore::Conflict(None),
        );
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "new editor content"
        );
    }

    #[test]
    fn a_save_after_the_claim_preserves_both_versions() {
        let root = tempfile::tempdir().unwrap();
        let archive = tempfile::tempdir_in(root.path()).unwrap();
        let relative = Path::new("note.txt");
        let target = rewind_file_path(root.path(), relative).unwrap();
        std::fs::write(&target, "reviewed").unwrap();
        let captured = claim_rewind_file(&target, archive.path()).unwrap().unwrap();
        std::fs::write(&target, "new editor content").unwrap();
        let mut staged = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        staged.write_all(b"restoration").unwrap();
        assert_eq!(
            finish_rewind_claim(
                root.path(),
                relative,
                &target,
                captured.clone(),
                "reviewed",
                Some(staged)
            )
            .unwrap(),
            RewindFileRestore::Conflict(Some(captured.clone())),
        );
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "new editor content"
        );
        assert_eq!(std::fs::read_to_string(captured).unwrap(), "reviewed");
    }

    #[test]
    fn a_successful_deletion_keeps_a_safety_copy_of_the_displaced_file() {
        let root = tempfile::tempdir().unwrap();
        let archive = tempfile::tempdir_in(root.path()).unwrap();
        let relative = Path::new("note.txt");
        let target = rewind_file_path(root.path(), relative).unwrap();
        std::fs::write(&target, "reviewed").unwrap();
        assert_eq!(
            restore_rewind_file(
                root.path(),
                relative,
                Some("reviewed"),
                None,
                archive.path()
            )
            .unwrap(),
            RewindFileRestore::Restored {
                preserved_edit: None
            },
        );
        assert!(!target.exists());
        let copy = std::fs::read_dir(archive.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join(relative);
        assert_eq!(std::fs::read_to_string(copy).unwrap(), "reviewed");
    }
}
