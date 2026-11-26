use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

use gix::ObjectId;
use gix::refs::TargetRef;

use super::super::dirs::read_dir_if_present;
use super::super::probe::{oids_to_stdin, run_probe};
use super::refs::{RefsUnreadable, visit_refs};
use super::{Captured, KeepReason, Ownership, probe_command};

#[cfg(test)]
pub(super) fn find_reflog_only_commits(
    worktree: &Path,
    repo: &gix::Repository,
) -> Option<KeepReason> {
    let discarded = match reflog_tips_or_keep(repo) {
        Ok(discarded) => discarded,
        Err(reason) => return Some(reason),
    };
    if discarded.is_empty() {
        return None;
    }
    let revisions = oids_to_stdin(discarded);
    let mut command = probe_command(worktree);
    command.args([
        "rev-list",
        "--max-count=1",
        "--ignore-missing",
        "--stdin",
        "--not",
        "--all",
        "--",
    ]);
    probe_reachability(worktree, command, revisions, KeepReason::Unpushed)
}

pub(super) fn find_unpushed_commits(
    worktree: &Path,
    ownership: Ownership,
    captured: Captured<'_>,
) -> Option<KeepReason> {
    let mut include: Vec<&str> = vec!["HEAD"];
    let mut exclude: Vec<&str> = vec!["--remotes"];
    if let Captured::WorkingTreeSnapshot(snapshot) = captured {
        exclude.push(snapshot);
    }
    match ownership {
        Ownership::Linked => exclude.extend(["--branches", "--tags"]),
        Ownership::Standalone => include.extend(["--branches", "--tags", "--reflog"]),
    }
    let mut command = probe_command(worktree);
    command.args(["rev-list", "--max-count=1"]);
    command.args(&include);
    command.arg("--not");
    command.args(&exclude);
    command.arg("--");
    probe_reachability(worktree, command, Vec::new(), KeepReason::Unpushed)
}

pub(super) fn probe_reachability(
    dir: &Path,
    command: Command,
    revisions: Vec<u8>,
    found: KeepReason,
) -> Option<KeepReason> {
    match run_probe(dir, command, revisions, "rev-list") {
        Ok(output) => (!output.stdout.trim_ascii().is_empty()).then_some(found),
        Err(_) => Some(KeepReason::CheckFailed),
    }
}

pub(super) fn find_missing_objects(
    repo: &gix::Repository,
    surviving: &gix::Repository,
) -> Option<KeepReason> {
    let (Ok(named), Ok(held)) = (find_ref_targets(repo), collect_ref_ids(surviving)) else {
        return Some(KeepReason::CheckFailed);
    };
    let discarded = match reflog_tips_or_keep(repo) {
        Ok(discarded) => discarded,
        Err(reason) => return Some(reason),
    };
    let mut include: Vec<ObjectId> = Vec::new();
    let mut seen = HashSet::new();
    let tips = named
        .into_iter()
        .map(|(name, id)| (Some(name), id))
        .chain(discarded.into_iter().map(|id| (None, id)));
    for (name, id) in tips {
        if held.contains(&id) || !seen.insert(id) {
            continue;
        }
        if !surviving.has_object(id) {
            match &name {
                Some(name) => tracing::info!(
                    git_dir = %repo.git_dir().display(),
                    ref_name = %name,
                    object = %id,
                    "surviving repo is missing an object this worktree names"
                ),
                None => tracing::info!(
                    git_dir = %repo.git_dir().display(),
                    object = %id,
                    "surviving repo is missing a reflog-only commit this worktree names"
                ),
            }
            return Some(KeepReason::OnlyCopy);
        }
        include.push(id);
    }
    if include.is_empty() {
        return None;
    }
    let revisions = oids_to_stdin(include.iter().copied());
    tracing::debug!(
        git_dir = %repo.git_dir().display(),
        surviving = %surviving.git_dir().display(),
        revisions = include.len(),
        "checking which objects the surviving repo cannot reach"
    );
    let surviving_dir = surviving.git_dir();
    let mut command = probe_command(surviving_dir);
    command.env("GIT_DIR", surviving_dir);
    command.env("GIT_WORK_TREE", surviving_dir);
    command.args([
        "rev-list",
        "--max-count=1",
        "--stdin",
        "--not",
        "--all",
        "--reflog",
        "--",
    ]);
    probe_reachability(surviving_dir, command, revisions, KeepReason::OnlyCopy)
}

fn collect_ref_ids(repo: &gix::Repository) -> Result<HashSet<ObjectId>, RefsUnreadable> {
    let mut ids = HashSet::new();
    visit_refs(repo, |reference| {
        if let TargetRef::Object(id) = reference.target() {
            ids.insert(id.to_owned());
        }
    })?;
    Ok(ids)
}

fn find_ref_targets(repo: &gix::Repository) -> Result<Vec<(String, ObjectId)>, RefsUnreadable> {
    let mut targets = Vec::new();
    visit_refs(repo, |reference| {
        if let TargetRef::Object(id) = reference.target() {
            targets.push((reference.name().as_bstr().to_string(), id.to_owned()));
        }
    })?;
    targets.extend(collect_child_worktree_heads(repo)?);
    if let Ok(head) = repo.head_id() {
        targets.push(("HEAD".to_string(), head.detach()));
    } else if let Err(error) = repo.head() {
        tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to read HEAD");
        return Err(RefsUnreadable);
    }
    Ok(targets)
}

fn reflog_tips_or_keep(repo: &gix::Repository) -> Result<HashSet<ObjectId>, KeepReason> {
    collect_reflog_tips(repo.git_dir()).map_err(|error| {
        tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to read reflogs");
        KeepReason::CheckFailed
    })
}

pub(crate) fn collect_reflog_tips(git_dir: &Path) -> std::io::Result<HashSet<ObjectId>> {
    if git_dir.join("reftable").try_exists()? {
        return Err(std::io::Error::other(
            "reftable repositories store reflogs in a format this reader does not support",
        ));
    }
    let mut tips = HashSet::new();
    let mut pending = vec![git_dir.join("logs")];
    if let Some(entries) = read_dir_if_present(&git_dir.join("worktrees"))? {
        for entry in entries {
            pending.push(entry?.path().join("logs"));
        }
    }
    while let Some(path) = pending.pop() {
        if path.ends_with("refs/remotes") {
            continue;
        }
        if path.is_dir() {
            let Some(entries) = read_dir_if_present(&path)? else {
                continue;
            };
            for entry in entries {
                pending.push(entry?.path());
            }
            continue;
        }
        let log = match std::fs::read(&path) {
            Ok(log) => log,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let oids = log
            .split(|byte| *byte == b'\n')
            .flat_map(|line| line.split(|byte| *byte == b' ').take(2));
        for oid in oids {
            if let Ok(id) = ObjectId::from_hex(oid)
                && !id.is_null()
            {
                tips.insert(id);
            }
        }
    }
    Ok(tips)
}

/// Child-worktree HEADs, so a linked worktree's tip is compared against the
/// surviving repo. An unreadable registration is "couldn't tell" and fails
/// toward keep (`RefsUnreadable`); a missing `HEAD`/dir is skipped.
fn collect_child_worktree_heads(
    repo: &gix::Repository,
) -> Result<Vec<(String, ObjectId)>, RefsUnreadable> {
    let registrations = repo.git_dir().join("worktrees");
    let entries = match read_dir_if_present(&registrations) {
        Ok(None) => return Ok(Vec::new()),
        Ok(Some(entries)) => entries,
        Err(error) => {
            tracing::warn!(path = %registrations.display(), %error, "failed to read worktrees registrations");
            return Err(RefsUnreadable);
        }
    };
    let mut heads = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            tracing::warn!(path = %registrations.display(), %error, "failed to read a worktree registration entry");
            RefsUnreadable
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        match std::fs::read_to_string(entry.path().join("HEAD")) {
            Ok(head) => {
                if let Ok(id) = ObjectId::from_hex(head.trim().as_bytes()) {
                    heads.push((format!("worktrees/{name}/HEAD"), id));
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(path = %entry.path().display(), %error, "failed to read a child worktree HEAD");
                return Err(RefsUnreadable);
            }
        }
    }
    Ok(heads)
}
