//! Reviewed file changes and a durable record for undoing the latest file rewind.

use crate::session::{RewindConflictInfo, RewindMode, RewindResponse};
use echo_agent_workspace::file_system::{RewindFileRestore, read_rewind_file, restore_rewind_file};
use echo_agent_workspace::session::file_state::RewindPoint;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct RewindFileChange {
    before: Option<String>,
    after: Option<String>,
    continuous: bool,
}

pub(super) struct ProtectedRewind {
    pub(super) revision: String,
    pub(super) ready: BTreeMap<PathBuf, RewindFileChange>,
    pub(super) conflicts: Vec<RewindConflictInfo>,
}

const REWIND_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct RewindJournal {
    operation: String,
    root: PathBuf,
    files: BTreeMap<PathBuf, RewindFileChange>,
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct RewindProgress {
    operation: String,
    pending: Option<PathBuf>,
    applied: BTreeSet<PathBuf>,
    uncertain: BTreeSet<PathBuf>,
}

fn read_rewind_record<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        meta.is_file() && meta.len() <= REWIND_JOURNAL_BYTES,
        "无法读取回溯记录"
    );
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "无法读取回溯记录");
    let mut data = Vec::new();
    file.take(REWIND_JOURNAL_BYTES + 1).read_to_end(&mut data)?;
    anyhow::ensure!(
        data.len() as u64 <= REWIND_JOURNAL_BYTES,
        "回溯记录超过大小上限"
    );
    Ok(Some(serde_json::from_slice(&data)?))
}

fn save_rewind_record<T: serde::Serialize>(path: &Path, record: &T) -> anyhow::Result<()> {
    let data = serde_json::to_vec(record)?;
    anyhow::ensure!(
        data.len() as u64 <= REWIND_JOURNAL_BYTES,
        "回溯记录超过大小上限"
    );
    let mut file = tempfile::NamedTempFile::new_in(path.parent().expect("session directory"))?;
    file.write_all(&data)?;
    crate::session::storage::sync_file_durable(file.as_file())?;
    file.persist(path)?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().expect("session directory"))?.sync_all()?;
    Ok(())
}

pub(super) fn save_new_rewind(
    root: &Path,
    session_dir: &Path,
    files: BTreeMap<PathBuf, RewindFileChange>,
) -> anyhow::Result<RewindJournal> {
    let journal = RewindJournal {
        operation: uuid::Uuid::new_v4().to_string(),
        root: root.canonicalize()?,
        files,
    };
    save_rewind_record(&session_dir.join("file-rewind.json"), &journal)?;
    Ok(journal)
}

fn apply_rewind_files(
    session_dir: &Path,
    journal: &RewindJournal,
    paths: impl IntoIterator<Item = PathBuf>,
    progress: &mut RewindProgress,
    undo: bool,
    conflicts: &mut Vec<RewindConflictInfo>,
) -> anyhow::Result<Vec<String>> {
    let progress_path = session_dir.join("file-rewind-progress.json");
    let mut reverted = Vec::new();
    for path in paths {
        let change = &journal.files[&path];
        let (expected, desired) = if undo {
            (&change.before, &change.after)
        } else {
            (&change.after, &change.before)
        };
        progress.pending = Some(path.clone());
        save_rewind_record(&progress_path, progress)?;
        let result = restore_rewind_file(
            &journal.root,
            &path,
            expected.as_deref(),
            desired.as_deref(),
            session_dir,
        );
        progress.pending = None;
        let label = path.to_string_lossy().into_owned();
        match result {
            Ok(RewindFileRestore::Restored { preserved_edit }) => {
                if undo {
                    progress.applied.remove(&path);
                } else {
                    progress.applied.insert(path);
                }
                if let Some(copy) = preserved_edit {
                    conflicts.push(RewindConflictInfo {
                        path: label.clone(),
                        conflict_type: "concurrent_copy_preserved".into(),
                        recovery_path: Some(copy.to_string_lossy().into_owned()),
                    });
                }
                reverted.push(label);
            }
            Ok(RewindFileRestore::Conflict(copy)) => conflicts.push(RewindConflictInfo {
                path: label,
                conflict_type: "modified_externally".into(),
                recovery_path: copy.map(|copy| copy.to_string_lossy().into_owned()),
            }),
            Err(error) => {
                tracing::warn!(%error, path = %label, "could not restore reviewed file");
                conflicts.push(RewindConflictInfo {
                    path: label,
                    conflict_type: "restore_failed".into(),
                    recovery_path: None,
                });
            }
        }
        save_rewind_record(&progress_path, progress)?;
    }
    Ok(reverted)
}

pub(super) fn apply_file_rewind(
    session_dir: &Path,
    journal: &RewindJournal,
    conflicts: &mut Vec<RewindConflictInfo>,
) -> anyhow::Result<Vec<String>> {
    let mut progress = RewindProgress {
        operation: journal.operation.clone(),
        ..Default::default()
    };
    let paths = journal.files.keys().cloned().collect::<Vec<_>>();
    apply_rewind_files(session_dir, journal, paths, &mut progress, false, conflicts)
        .map_err(|error| anyhow::anyhow!("回溯记录写入失败，文件可能已部分恢复：{error}"))
}

pub(super) fn rewind_undo(
    root: &Path,
    session_dir: &Path,
    execute: bool,
    revision: Option<&str>,
) -> anyhow::Result<RewindResponse> {
    let mut response = RewindResponse {
        success: false,
        target_prompt_index: 0,
        mode: RewindMode::FilesOnly,
        reverted_files: vec![],
        clean_files: vec![],
        conflicts: vec![],
        prompt_text: None,
        error: None,
        preview_revision: None,
    };
    let Some(journal) = read_rewind_record::<RewindJournal>(&session_dir.join("file-rewind.json"))?
    else {
        return Ok(response);
    };
    let Some(mut progress) =
        read_rewind_record::<RewindProgress>(&session_dir.join("file-rewind-progress.json"))?
            .filter(|progress| progress.operation == journal.operation)
    else {
        return Ok(response);
    };
    anyhow::ensure!(
        journal.root == root.canonicalize()?,
        "工作目录已变化，无法撤销文件回溯"
    );
    if let Some(pending) = progress.pending.take() {
        progress.uncertain.insert(pending);
    }
    let candidates: BTreeSet<_> = progress
        .applied
        .union(&progress.uncertain)
        .cloned()
        .collect();
    if candidates.is_empty() {
        return Ok(response);
    }
    let mut ready = Vec::new();
    let mut hash = Sha256::new();
    hash.update(journal.operation.as_bytes());
    for path in candidates {
        let Some(change) = journal.files.get(&path) else {
            anyhow::bail!("回溯记录与文件清单不一致");
        };
        let label = path.to_string_lossy().into_owned();
        let kind = if progress.uncertain.contains(&path) {
            "rewind_interrupted"
        } else {
            match read_rewind_file(root, &path) {
                Ok(current) if current == change.before => "ready",
                Ok(_) => "modified_externally",
                Err(_) => "unavailable",
            }
        };
        hash.update((path.as_os_str().len() as u64).to_le_bytes());
        hash.update(path.as_os_str().as_encoded_bytes());
        hash.update(kind.as_bytes());
        if kind == "ready" {
            ready.push(path);
            response.clean_files.push(label);
        } else {
            response.conflicts.push(RewindConflictInfo {
                path: label,
                conflict_type: kind.into(),
                recovery_path: None,
            });
        }
    }
    let token = format!("{:x}", hash.finalize());
    if !execute {
        response.preview_revision = Some(token);
        return Ok(response);
    }
    anyhow::ensure!(
        revision == Some(token.as_str()),
        "撤销预览已失效，请重新核对文件"
    );
    response.reverted_files = apply_rewind_files(
        session_dir,
        &journal,
        ready,
        &mut progress,
        true,
        &mut response.conflicts,
    )?;
    response.success = true;
    Ok(response)
}

pub(super) fn protected_rewind(
    root: &Path,
    points: &[RewindPoint],
    session: &str,
    current: usize,
    target: usize,
    mode: RewindMode,
) -> ProtectedRewind {
    let mut changes = BTreeMap::<PathBuf, RewindFileChange>::new();
    for point in points.iter().filter(|point| point.prompt_index >= target) {
        for (path, before) in &point.file_snapshots {
            let relative = path.try_to_relative(root).as_path().to_owned();
            let after = point.after_snapshots.get(path);
            let change = changes.entry(relative).or_insert_with(|| RewindFileChange {
                before: before.content.clone(),
                after: before.content.clone(),
                continuous: true,
            });
            change.continuous &=
                after.is_some_and(|image| image.tool_verified) && change.after == before.content;
            change.after = after.and_then(|snapshot| snapshot.content.clone());
        }
    }

    let mut ready = BTreeMap::new();
    let mut conflicts = Vec::new();
    let mut revision = Sha256::new();
    revision.update(
        serde_json::to_vec(&(session, current, target, mode)).expect("rewind identity serializes"),
    );
    for (path, change) in changes {
        if change.continuous && change.before == change.after {
            continue;
        }
        let kind = match read_rewind_file(root, &path) {
            _ if !change.continuous => "unverified_history",
            Ok(current) if current == change.before => continue,
            Ok(current) if current == change.after => "ready",
            Ok(None) => "deleted_externally",
            Ok(Some(_)) if change.after.is_none() => "created_externally",
            Ok(Some(_)) => "modified_externally",
            Err(_) => "unavailable",
        };
        // Hash the immutable plan in canonical order. The token contains no file contents.
        revision.update((path.as_os_str().len() as u64).to_le_bytes());
        revision.update(path.as_os_str().as_encoded_bytes());
        revision.update(kind.as_bytes());
        for content in [&change.before, &change.after] {
            revision.update([u8::from(content.is_some())]);
            revision.update(Sha256::digest(
                content.as_deref().unwrap_or_default().as_bytes(),
            ));
        }
        if kind == "ready" {
            ready.insert(path, change);
        } else {
            conflicts.push(RewindConflictInfo {
                path: path.to_string_lossy().into_owned(),
                conflict_type: kind.into(),
                recovery_path: None,
            });
        }
    }
    ProtectedRewind {
        revision: format!("{:x}", revision.finalize()),
        ready,
        conflicts,
    }
}

#[cfg(test)]
mod protected_rewind_tests {
    use super::*;
    use echo_agent_workspace::session::file_state::{FileSnapshot, FlexiblePath};

    #[test]
    fn durable_undo_preserves_user_changes_and_flags_an_interrupted_file() {
        let workspace = tempfile::tempdir().unwrap();
        let session = tempfile::tempdir().unwrap();
        let root = workspace.path();
        let dir = session.path();
        let files = ["ready.txt", "changed.txt", "interrupted.txt"]
            .into_iter()
            .map(|name| {
                (
                    PathBuf::from(name),
                    RewindFileChange {
                        before: Some("prior".into()),
                        after: Some("agent".into()),
                        continuous: true,
                    },
                )
            })
            .collect();
        for name in ["ready.txt", "changed.txt", "interrupted.txt"] {
            std::fs::write(root.join(name), "agent").unwrap();
        }
        let journal = save_new_rewind(root, dir, files).unwrap();
        let mut progress = RewindProgress {
            operation: journal.operation.clone(),
            ..Default::default()
        };
        let mut conflicts = vec![];
        let restored = apply_rewind_files(
            dir,
            &journal,
            ["ready.txt", "changed.txt"].into_iter().map(PathBuf::from),
            &mut progress,
            false,
            &mut conflicts,
        )
        .unwrap();
        assert_eq!(restored, ["ready.txt", "changed.txt"]);
        assert!(conflicts.is_empty());

        // Simulate a process exiting between recording intent and confirming the result.
        progress.pending = Some("interrupted.txt".into());
        save_rewind_record(&dir.join("file-rewind-progress.json"), &progress).unwrap();
        std::fs::write(root.join("interrupted.txt"), "prior").unwrap();
        std::fs::write(root.join("changed.txt"), "user").unwrap();
        drop(journal);

        let preview = rewind_undo(root, dir, false, None).unwrap();
        assert_eq!(preview.clean_files, ["ready.txt"]);
        assert_eq!(preview.conflicts.len(), 2);
        assert!(
            preview
                .conflicts
                .iter()
                .any(|file| file.conflict_type == "rewind_interrupted")
        );
        let token = preview.preview_revision.as_deref();
        std::fs::write(root.join("ready.txt"), "newer user edit").unwrap();
        assert!(rewind_undo(root, dir, true, token).is_err());
        std::fs::write(root.join("ready.txt"), "prior").unwrap();
        assert_eq!(
            rewind_undo(root, dir, true, token).unwrap().reverted_files,
            ["ready.txt"]
        );
        assert_eq!(
            std::fs::read_to_string(root.join("ready.txt")).unwrap(),
            "agent"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("changed.txt")).unwrap(),
            "user"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("interrupted.txt")).unwrap(),
            "prior"
        );
    }

    #[test]
    fn reviewed_files_are_bound_to_their_versions_and_old_unknown_snapshots_are_excluded() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        let mut point = RewindPoint::new(0);
        for name in ["ready.txt", "changed.txt", "legacy.txt"] {
            let path = FlexiblePath::Absolute(root.join(name));
            point.add_snapshot(FileSnapshot::new_flexible(
                path.clone(),
                Some("before".into()),
            ));
            if name != "legacy.txt" {
                let mut after = FileSnapshot::new_flexible(path, Some("agent".into()));
                after.tool_verified = true;
                point.set_after_snapshot(after);
            }
        }
        std::fs::write(root.join("ready.txt"), "agent").unwrap();
        std::fs::write(root.join("changed.txt"), "user").unwrap();
        std::fs::write(root.join("legacy.txt"), "user").unwrap();
        let check = || {
            protected_rewind(
                root,
                std::slice::from_ref(&point),
                "session",
                1,
                0,
                RewindMode::FilesOnly,
            )
        };
        let preview = check();
        assert_eq!(preview.ready.len(), 1);
        assert!(preview.ready.contains_key(Path::new("ready.txt")));
        assert_eq!(preview.conflicts.len(), 2);
        assert!(
            preview
                .conflicts
                .iter()
                .any(|item| item.conflict_type == "unverified_history")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("ready.txt")).unwrap(),
            "agent"
        );
        std::fs::write(root.join("ready.txt"), "edited after preview").unwrap();
        assert_ne!(check().revision, preview.revision);
        assert_eq!(
            restore_rewind_file(
                root,
                Path::new("ready.txt"),
                Some("agent"),
                Some("before"),
                root
            )
            .unwrap(),
            RewindFileRestore::Conflict(None),
        );
        assert_eq!(
            std::fs::read_to_string(root.join("ready.txt")).unwrap(),
            "edited after preview"
        );
    }
}
