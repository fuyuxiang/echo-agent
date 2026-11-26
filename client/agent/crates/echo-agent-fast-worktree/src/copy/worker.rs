//! Worker logic for replicating a single filesystem entry.

use std::collections::HashSet;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::{DashMap, DashSet};

use crate::copy::cow;
use crate::copy::types::{CopyEntry, CopyEntryKind};

pub(crate) struct WorkerCtx {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub files_copied: Arc<AtomicU64>,
    pub dirs_created: Arc<AtomicU64>,
    pub symlinks_copied: Arc<AtomicU64>,
    pub issues: Arc<std::sync::Mutex<Vec<String>>>,
    pub copied_paths: Arc<DashSet<PathBuf>>,
    pub file_metadata: Arc<DashMap<PathBuf, Metadata>>,
}

pub(crate) fn run_worker(rx: crossbeam::channel::Receiver<CopyEntry>, ctx: WorkerCtx) {
    // Track created directories to avoid redundant mkdir calls.
    let mut created_dirs: HashSet<PathBuf> = HashSet::new();

    for entry in rx {
        let src = ctx.source.join(&entry.rel_path);
        let dst = ctx.dest.join(&entry.rel_path);

        let success = process_entry(
            &entry,
            &src,
            &dst,
            &mut created_dirs,
            &ctx.files_copied,
            &ctx.dirs_created,
            &ctx.symlinks_copied,
            &ctx.issues,
            &ctx.file_metadata,
        );

        if success {
            ctx.copied_paths.insert(entry.rel_path);
        }
    }
}

fn process_entry(
    entry: &CopyEntry,
    src: &Path,
    dst: &Path,
    created_dirs: &mut HashSet<PathBuf>,
    files_copied: &AtomicU64,
    dirs_created: &AtomicU64,
    symlinks_copied: &AtomicU64,
    issues: &std::sync::Mutex<Vec<String>>,
    file_metadata: &DashMap<PathBuf, Metadata>,
) -> bool {
    // Ensure parent directory exists.
    if let Some(parent) = dst.parent()
        && !parent.as_os_str().is_empty()
        && created_dirs.insert(parent.to_path_buf())
        && let Err(e) = std::fs::create_dir_all(parent)
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        issues
            .lock()
            .unwrap()
            .push(format!("mkdir {}: {}", parent.display(), e));
        return false;
    }

    match entry.kind {
        CopyEntryKind::Dir => match std::fs::create_dir_all(dst) {
            Ok(()) => {
                dirs_created.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => true,
            Err(e) => {
                issues
                    .lock()
                    .unwrap()
                    .push(format!("mkdir {}: {}", entry.rel_path.display(), e));
                false
            }
        },
        CopyEntryKind::Symlink => match std::fs::read_link(src) {
            Ok(target) => match cow::replace_symlink(&target, dst) {
                Ok(()) => {
                    symlinks_copied.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(e) => {
                    issues.lock().unwrap().push(format!(
                        "symlink {}: {}",
                        entry.rel_path.display(),
                        e
                    ));
                    false
                }
            },
            Err(e) => {
                issues.lock().unwrap().push(format!(
                    "read_link {}: {}",
                    entry.rel_path.display(),
                    e
                ));
                false
            }
        },
        CopyEntryKind::File => match cow::clone_file(src, dst) {
            Ok(()) => {
                files_copied.fetch_add(1, Ordering::Relaxed);

                // Collect file metadata for index updates
                if let Ok(metadata) = std::fs::metadata(dst) {
                    file_metadata.insert(entry.rel_path.clone(), metadata);
                }

                true
            }
            Err(e) => {
                issues
                    .lock()
                    .unwrap()
                    .push(format!("copy {}: {}", entry.rel_path.display(), e));
                false
            }
        },
    }
}
