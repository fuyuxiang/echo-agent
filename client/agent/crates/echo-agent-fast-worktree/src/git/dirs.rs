use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Cap on entries walked in one `find_missing_file` scan. A fully-covered store
/// is the worst case (it walks everything before answering `None`); on a large
/// LFS/submodule tree that is a syscall storm inside the gate's time budget. On
/// exhaustion we return an error, which every caller reads as "couldn't tell"
/// and biases to keep — never a delete.
const MAX_ENTRIES_SCANNED: usize = 100_000;

/// The first file under `ours` (recursively) that `theirs` does not also hold at
/// the same relative path. `None` means the survivor covers everything here.
/// `Err` on any read error or when the scan exceeds `MAX_ENTRIES_SCANNED`.
pub(crate) fn find_missing_file(
    ours: &Path,
    theirs: Option<&Path>,
) -> std::io::Result<Option<PathBuf>> {
    let mut pending = vec![PathBuf::new()];
    let mut scanned = 0usize;
    while let Some(prefix) = pending.pop() {
        let Some(entries) = read_dir_if_present(&ours.join(&prefix))? else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            scanned += 1;
            if scanned > MAX_ENTRIES_SCANNED {
                return Err(std::io::Error::other("store scan exceeded entry budget"));
            }
            let relative = prefix.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                pending.push(relative);
            } else if !theirs.is_some_and(|theirs| theirs.join(&relative).exists()) {
                return Ok(Some(relative));
            }
        }
    }
    Ok(None)
}

pub(crate) fn pruned_path_holds_something(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_symlink())
        || has_any_file(path).unwrap_or(true)
}

pub(crate) fn has_any_file(directory: &Path) -> std::io::Result<bool> {
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Some(entries) = read_dir_if_present(&directory)? else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub(crate) fn read_dir_if_present(directory: &Path) -> std::io::Result<Option<std::fs::ReadDir>> {
    match std::fs::read_dir(directory) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn has_entry_outside(directory: &Path, removable: &[&str]) -> std::io::Result<bool> {
    let Some(entries) = read_dir_if_present(directory)? else {
        return Ok(false);
    };
    for entry in entries {
        let name = entry?.file_name();
        if !removable.contains(&name.to_string_lossy().as_ref()) {
            return Ok(true);
        }
    }
    Ok(false)
}
