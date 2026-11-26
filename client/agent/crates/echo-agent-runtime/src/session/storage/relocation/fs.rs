//! Durable filesystem operations used by session relocation.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use super::journal::{AtomicWriteFault, WriteFailure};
use super::{RelocationError, Result};

pub(super) fn require_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|e| io_error("inspect", path, e))?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(RelocationError::Inconsistent(format!(
            "expected directory: {}",
            path.display()
        )))
    }
}

pub(super) fn copy_directory(source: &Path, target: &Path) -> Result<()> {
    let source = dunce::canonicalize(source).map_err(|e| io_error("resolve", source, e))?;
    require_directory(&source)?;
    let target = resolve_path(target)?;
    if target.starts_with(&source) {
        return Err(RelocationError::Inconsistent(format!(
            "copy target must not equal or be nested under source: {}",
            target.display()
        )));
    }
    copy_new_directory(&source, &target)
}

fn resolve_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| io_error("resolve", path, e))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(_) | Component::Prefix(_) | Component::RootDir => {
                normalized.push(component.as_os_str());
            }
        }
    }

    let mut ancestor = normalized.as_path();
    let mut tail = Vec::new();
    loop {
        match dunce::canonicalize(ancestor) {
            Ok(mut resolved) => {
                resolved.extend(tail.into_iter().rev());
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let file_name = ancestor.file_name().ok_or_else(|| {
                    RelocationError::Inconsistent(format!(
                        "path has no resolvable ancestor: {}",
                        path.display()
                    ))
                })?;
                tail.push(file_name.to_owned());
                ancestor = ancestor.parent().ok_or_else(|| {
                    RelocationError::Inconsistent(format!(
                        "path has no resolvable ancestor: {}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => return Err(io_error("resolve", ancestor, error)),
        }
    }
}

fn copy_new_directory(source: &Path, target: &Path) -> Result<()> {
    create_new_dir_durable(target)?;
    copy_directory_contents(source, target)?;
    let permissions = fs::symlink_metadata(source)
        .map_err(|e| io_error("inspect", source, e))?
        .permissions();
    fs::set_permissions(target, permissions).map_err(|e| io_error("chmod", target, e))?;
    sync_dir(target).map_err(|e| io_error("sync", target, e))
}

fn copy_directory_contents(source: &Path, target: &Path) -> Result<()> {
    for entry in fs::read_dir(source).map_err(|e| io_error("read", source, e))? {
        let entry = entry.map_err(|e| io_error("read", source, e))?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let metadata =
            fs::symlink_metadata(&source_path).map_err(|e| io_error("inspect", &source_path, e))?;
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            copy_new_directory(&source_path, &target_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path).map_err(|e| io_error("copy", &source_path, e))?;
            fs::set_permissions(&target_path, metadata.permissions())
                .map_err(|e| io_error("chmod", &target_path, e))?;
            File::open(&target_path)
                .and_then(|file| super::super::sync_file_durable(&file))
                .map_err(|e| io_error("sync", &target_path, e))?;
        } else if file_type.is_symlink() {
            copy_symlink(&source_path, &target_path, &file_type)?;
        } else {
            return Err(RelocationError::Inconsistent(format!(
                "unsupported session entry: {}",
                source_path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &Path, target: &Path, _file_type: &fs::FileType) -> Result<()> {
    let link = fs::read_link(source).map_err(|e| io_error("readlink", source, e))?;
    std::os::unix::fs::symlink(link, target).map_err(|e| io_error("symlink", target, e))
}

#[cfg(windows)]
fn copy_symlink(source: &Path, target: &Path, file_type: &fs::FileType) -> Result<()> {
    use std::os::windows::fs::FileTypeExt;

    let link = fs::read_link(source).map_err(|e| io_error("readlink", source, e))?;
    if file_type.is_symlink_dir() {
        std::os::windows::fs::symlink_dir(link, target)
    } else if file_type.is_symlink_file() {
        std::os::windows::fs::symlink_file(link, target)
    } else {
        return Err(RelocationError::Inconsistent(
            "unknown Windows symlink type".into(),
        ));
    }
    .map_err(|e| io_error("symlink", target, e))
}

pub(super) fn remove_dir_durable(path: &Path) -> Result<()> {
    remove_dir(path)?;
    let parent = path.parent().ok_or_else(|| {
        RelocationError::Inconsistent(format!("directory has no parent: {}", path.display()))
    })?;
    sync_dir(parent).map_err(|e| io_error("sync", parent, e))
}

fn remove_dir(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error("remove", path, e)),
    }
}

#[cfg(test)]
pub(super) fn remove_dir_with_barrier_fault(path: &Path) -> Result<()> {
    remove_dir(path)?;
    Err(RelocationError::Inconsistent(
        "injected directory removal barrier failure".into(),
    ))
}

pub(super) fn write_new_durable(
    path: &Path,
    bytes: &[u8],
    fault: Option<AtomicWriteFault>,
) -> std::result::Result<(), WriteFailure> {
    let parent = path.parent().ok_or_else(|| {
        WriteFailure::NotCommitted(RelocationError::Inconsistent(format!(
            "path has no parent: {}",
            path.display()
        )))
    })?;
    let temp_path = temp_sibling(path);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut temp = options
            .open(&temp_path)
            .map_err(|e| WriteFailure::NotCommitted(io_error("create", &temp_path, e)))?;
        temp.write_all(bytes)
            .and_then(|()| super::super::sync_file_durable(&temp))
            .map_err(|e| WriteFailure::NotCommitted(io_error("write", &temp_path, e)))?;
        if fault == Some(AtomicWriteFault::BeforeRename) {
            return Err(WriteFailure::NotCommitted(RelocationError::Inconsistent(
                "injected pre-rename failure".into(),
            )));
        }
        rename_no_replace(&temp_path, path).map_err(WriteFailure::NotCommitted)?;
        if fault == Some(AtomicWriteFault::AfterRename) {
            return Err(WriteFailure::Committed(RelocationError::Inconsistent(
                "injected directory barrier failure".into(),
            )));
        }
        sync_dir(parent).map_err(|e| WriteFailure::Committed(io_error("sync", parent, e)))
    })();
    let _ = fs::remove_file(&temp_path);
    result
}

pub(super) fn write_atomic_durable(
    path: &Path,
    bytes: &[u8],
    permissions: Option<fs::Permissions>,
    fault: Option<AtomicWriteFault>,
) -> std::result::Result<(), WriteFailure> {
    let parent = path.parent().ok_or_else(|| {
        WriteFailure::NotCommitted(RelocationError::Inconsistent(format!(
            "path has no parent: {}",
            path.display()
        )))
    })?;
    create_dir_durable(parent).map_err(WriteFailure::NotCommitted)?;
    let temp_path = temp_sibling(path);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut temp = options
            .open(&temp_path)
            .map_err(|e| WriteFailure::NotCommitted(io_error("create", &temp_path, e)))?;
        if let Some(permissions) = permissions {
            temp.set_permissions(permissions)
                .map_err(|e| WriteFailure::NotCommitted(io_error("chmod", &temp_path, e)))?;
        }
        temp.write_all(bytes)
            .and_then(|()| super::super::sync_file_durable(&temp))
            .map_err(|e| WriteFailure::NotCommitted(io_error("write", &temp_path, e)))?;
        if fault == Some(AtomicWriteFault::BeforeRename) {
            return Err(WriteFailure::NotCommitted(RelocationError::Inconsistent(
                "injected pre-rename failure".into(),
            )));
        }
        fs::rename(&temp_path, path)
            .map_err(|e| WriteFailure::NotCommitted(io_error("persist", path, e)))?;
        if fault == Some(AtomicWriteFault::AfterRename) {
            return Err(WriteFailure::Committed(RelocationError::Inconsistent(
                "injected directory barrier failure".into(),
            )));
        }
        sync_dir(parent).map_err(|e| WriteFailure::Committed(io_error("sync", parent, e)))
    })();
    if matches!(&result, Err(WriteFailure::NotCommitted(_))) {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(super) fn rename_no_replace(source: &Path, target: &Path) -> Result<()> {
    use nix::fcntl::{AT_FDCWD, RenameFlags, renameat2};
    renameat2(
        AT_FDCWD,
        source,
        AT_FDCWD,
        target,
        RenameFlags::RENAME_NOREPLACE,
    )
    .map_err(|error| match error {
        nix::errno::Errno::EEXIST => RelocationError::Collision(target.to_path_buf()),
        nix::errno::Errno::EINVAL | nix::errno::Errno::ENOSYS | nix::errno::Errno::EOPNOTSUPP => {
            RelocationError::UnsupportedPublication
        }
        error => io_error("publish", target, io::Error::from(error)),
    })
}

#[cfg(target_os = "macos")]
pub(super) fn rename_no_replace(source: &Path, target: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let source_c = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| RelocationError::Inconsistent("source path contains NUL".into()))?;
    let target_c = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| RelocationError::Inconsistent("target path contains NUL".into()))?;
    // SAFETY: both pointers are live, NUL-terminated path strings for this call.
    if unsafe { libc::renamex_np(source_c.as_ptr(), target_c.as_ptr(), libc::RENAME_EXCL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EEXIST) => Err(RelocationError::Collision(target.to_path_buf())),
        Some(code) if code == libc::EINVAL || code == libc::ENOTSUP => {
            Err(RelocationError::UnsupportedPublication)
        }
        _ => Err(io_error("publish", target, error)),
    }
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
pub(super) fn rename_no_replace(_source: &Path, _target: &Path) -> Result<()> {
    Err(RelocationError::UnsupportedPublication)
}

fn create_new_dir_durable(path: &Path) -> Result<()> {
    fs::create_dir(path).map_err(|error| match error.kind() {
        io::ErrorKind::AlreadyExists => RelocationError::Collision(path.to_path_buf()),
        _ => io_error("create", path, error),
    })?;
    let parent = path.parent().ok_or_else(|| {
        RelocationError::Inconsistent(format!("directory has no parent: {}", path.display()))
    })?;
    sync_dir(parent).map_err(|e| io_error("sync", parent, e))
}

pub(super) fn create_dir_durable(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {
            let parent = path.parent().ok_or_else(|| {
                RelocationError::Inconsistent(format!(
                    "directory has no parent: {}",
                    path.display()
                ))
            })?;
            sync_dir(parent).map_err(|e| io_error("sync", parent, e))
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => require_directory(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                RelocationError::Inconsistent(format!(
                    "directory has no parent: {}",
                    path.display()
                ))
            })?;
            create_dir_durable(parent)?;
            create_dir_durable(path)
        }
        Err(e) => Err(io_error("create", path, e)),
    }
}

fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", uuid::Uuid::now_v7()));
    PathBuf::from(name)
}

pub(super) fn is_lock_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || (error.raw_os_error().is_some()
            && error.raw_os_error() == fs2::lock_contended_error().raw_os_error())
}

#[cfg(unix)]
pub(super) fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
pub(super) fn sync_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(super) fn io_error(operation: &'static str, path: &Path, source: io::Error) -> RelocationError {
    RelocationError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
