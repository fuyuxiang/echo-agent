use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::super::probe::run_probe;
use super::{KeepReason, probe_command};

fn probe_output(worktree: &Path, command: Command, what: &str) -> Result<Vec<u8>, KeepReason> {
    match run_probe(worktree, command, Vec::new(), what) {
        Ok(output) => Ok(output.stdout),
        Err(_) => Err(KeepReason::CheckFailed),
    }
}

/// Keep when the snapshot's stored blob for a path does not round-trip the
/// on-disk bytes: a clean filter or CRLF rule can make `ls-tree`'s blob differ
/// from the working-tree file, so a snapshot that looks complete would still
/// lose the real content. Compares the stored id against `hash-object
/// --no-filters` of the file (see `find_converted_path_in`).
pub(super) fn find_converted_path(worktree: &Path, snapshot: &str) -> Option<KeepReason> {
    let mut command = probe_command(worktree);
    command.args(["status", "--porcelain", "-z", "--untracked-files=all"]);
    let changed = match probe_output(worktree, command, "status") {
        Ok(changed) => changed,
        Err(reason) => return Some(reason),
    };
    let mut records = changed.split(|byte| *byte == 0);
    let mut paths: Vec<PathBuf> = Vec::new();
    while let Some(record) = records.next() {
        // A rename/copy emits its origin path as a second NUL record; consume it
        // so parsing stays aligned with the `XY <path>` records.
        if record
            .iter()
            .take(2)
            .any(|status| matches!(status, b'R' | b'C'))
        {
            records.next();
        }
        // Porcelain records are `XY <path>`: two status bytes, a space, path.
        let Some(path) = record.get(3..).filter(|path| !path.is_empty()) else {
            continue;
        };
        let path = gix::path::from_byte_slice(path).to_owned();
        // A stat error here would silently drop the path from the round-trip
        // comparison and could let the gate delete; keep instead. NotFound is
        // fine (status listed a file removed since).
        match std::fs::symlink_metadata(worktree.join(&path)) {
            Ok(meta) if meta.is_file() => paths.push(path),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(path = %worktree.display(), %error, "failed to stat a listed path; keeping");
                return Some(KeepReason::CheckFailed);
            }
        }
    }
    for chunk in paths.chunks(CONVERSION_CHUNK) {
        if let Some(reason) = find_converted_path_in(worktree, snapshot, chunk) {
            return Some(reason);
        }
    }
    None
}

const CONVERSION_CHUNK: usize = 256;

pub(super) fn hashes_line_up(raw: &[u8], asked: usize) -> Option<Vec<&[u8]>> {
    let lines: Vec<&[u8]> = raw.split(|byte| *byte == b'\n').collect();
    match lines.split_last() {
        Some((last, rest)) if last.is_empty() && rest.len() == asked => Some(rest.to_vec()),
        _ => None,
    }
}

fn find_converted_path_in(
    worktree: &Path,
    snapshot: &str,
    paths: &[PathBuf],
) -> Option<KeepReason> {
    let mut stored = probe_command(worktree);
    stored.args(["ls-tree", "-z", snapshot, "--"]);
    stored.args(paths);
    let stored = match probe_output(worktree, stored, "ls-tree") {
        Ok(stored) => stored,
        Err(reason) => return Some(reason),
    };
    let mut raw = probe_command(worktree);
    raw.args(["hash-object", "--no-filters", "--"]);
    raw.args(paths);
    let raw = match probe_output(worktree, raw, "hash-object") {
        Ok(raw) => raw,
        Err(reason) => return Some(reason),
    };
    let by_path: HashMap<&[u8], &[u8]> = stored
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let tab_index = record.iter().position(|byte| *byte == b'\t')?;
            let (head, path) = (&record[..tab_index], &record[tab_index + 1..]);
            Some((path, head.rsplit(|byte| *byte == b' ').next()?))
        })
        .collect();
    let Some(raw) = hashes_line_up(&raw, paths.len()) else {
        tracing::warn!(
            path = %worktree.display(),
            asked = paths.len(),
            "hash-object returned a different number of hashes than paths"
        );
        return Some(KeepReason::CheckFailed);
    };
    for (path, raw) in paths.iter().zip(raw) {
        let asked = gix::path::into_bstr(path.as_path());
        let Some(stored) = by_path.get(&asked as &[u8]) else {
            continue;
        };
        if *stored == raw {
            continue;
        }
        tracing::info!(
            path = %worktree.display(),
            file = %path.display(),
            "file content differs from the snapshot blob"
        );
        return Some(KeepReason::NotInSnapshot(path.display().to_string()));
    }
    None
}
