use std::path::{Path, PathBuf};

use super::super::dirs::{find_missing_file, has_any_file, has_entry_outside, read_dir_if_present};
use super::KeepReason;
use super::refs::visit_refs;

const REMOVABLE_REGISTRATION_ENTRIES: &[&str] = &[
    "HEAD",
    "ORIG_HEAD",
    "REBASE_HEAD",
    "AUTO_MERGE",
    "index",
    "commondir",
    "gitdir",
    "logs",
    "FETCH_HEAD",
    "COMMIT_EDITMSG",
    "MERGE_RR",
    "fsmonitor--daemon",
    "fsmonitor--daemon.ipc",
];

const REMOVABLE_REGISTRATION_PREFIX: &str = "sharedindex.";

const REMOVABLE_INFO_ENTRIES: &[&str] = &["sparse-checkout"];

const CHECKOUT_SHAPE_KEYS: &[&str] = &[
    "core.sparsecheckout",
    "core.sparsecheckoutcone",
    "index.sparse",
];

const OPERATION_IN_PROGRESS: &[&str] = &[
    "rebase-merge",
    "rebase-apply",
    "sequencer",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "BISECT_START",
];

pub(super) fn find_own_registration_state(git_dir: &Path) -> Option<KeepReason> {
    match read_state_entry(git_dir) {
        Ok(None) => None,
        Ok(Some(name)) => {
            tracing::info!(git_dir = %git_dir.display(), entry = %name, "keeping worktree: registration holds local state");
            Some(KeepReason::WorktreeLocalState(name))
        }
        Err(error) => keep_on_unreadable(git_dir, &error),
    }
}

/// A git directory that could not be read fails toward keep.
fn keep_on_unreadable(git_dir: &Path, error: &std::io::Error) -> Option<KeepReason> {
    tracing::warn!(git_dir = %git_dir.display(), %error, "failed to read git directory");
    Some(KeepReason::CheckFailed)
}

pub(super) fn find_repo_local_state(repo: &gix::Repository) -> Option<KeepReason> {
    find_local_refs(repo)
        .or_else(|| find_registered_child(repo))
        .or_else(|| find_operation_in_progress(repo.git_dir()))
}

/// Stores that die with `git_dir` and that no ref covers: `.git/modules` and LFS
/// objects. Standalone only; keep when `surviving` lacks a file. Path-matched, so
/// it over-keeps rather than risk dropping a last copy.
pub(super) fn find_dying_stores(
    repo: &gix::Repository,
    surviving: Option<&gix::Repository>,
) -> Option<KeepReason> {
    for store in DyingStore::ALL {
        // Both sides resolve from the same variant, so a store can never be
        // compared against the survivor's other store.
        let ours = store.path_in(repo);
        let theirs = surviving.map(|survivor| store.path_in(survivor));
        match find_missing_file(&ours, theirs.as_deref()) {
            Ok(None) => {}
            Ok(Some(missing)) => {
                let held = format!("{}/{}", store.label(), missing.display());
                tracing::info!(git_dir = %repo.git_dir().display(), held, "keeping worktree: a store dies with it");
                return Some(KeepReason::WorktreeLocalState(held));
            }
            Err(error) => {
                tracing::warn!(git_dir = %repo.git_dir().display(), store = store.label(), %error, "failed to read a store");
                return Some(KeepReason::CheckFailed);
            }
        }
    }
    None
}

#[derive(Clone, Copy)]
enum DyingStore {
    /// Submodule object stores under `.git/modules`.
    Submodules,
    /// The LFS objects a pointer stands for.
    LfsObjects,
}

impl DyingStore {
    const ALL: [DyingStore; 2] = [DyingStore::Submodules, DyingStore::LfsObjects];

    fn path_in(self, repo: &gix::Repository) -> PathBuf {
        match self {
            DyingStore::Submodules => repo.git_dir().join("modules"),
            DyingStore::LfsObjects => lfs_objects_dir(repo),
        }
    }

    /// Prefix for the kept-path report string.
    fn label(self) -> &'static str {
        match self {
            DyingStore::Submodules => "modules",
            DyingStore::LfsObjects => "lfs/objects",
        }
    }
}

/// Where git-lfs keeps the bytes its pointers stand for. `lfs.storage` moves it;
/// a relative value is read against the common directory.
fn lfs_objects_dir(repo: &gix::Repository) -> PathBuf {
    let config = repo.config_snapshot();
    let configured = config
        .string("lfs.storage")
        .map(|storage| gix::path::from_bstr(storage.as_ref()).into_owned());
    match configured {
        Some(storage) if storage.is_absolute() => storage.join("objects"),
        Some(storage) => repo.common_dir().join(storage).join("objects"),
        None => repo.common_dir().join("lfs").join("objects"),
    }
}

pub(super) fn find_local_refs(repo: &gix::Repository) -> Option<KeepReason> {
    const REACHABILITY_NAMESPACES: &[&[u8]] = &[b"refs/heads/", b"refs/tags/", b"refs/remotes/"];
    let mut outside = None;
    if visit_refs(repo, |reference| {
        let name = reference.name().as_bstr();
        if outside.is_none()
            && !REACHABILITY_NAMESPACES
                .iter()
                .any(|namespace| name.starts_with(namespace))
        {
            outside = Some(name.to_string());
        }
    })
    .is_err()
    {
        return Some(KeepReason::CheckFailed);
    }
    let name = outside?;
    tracing::info!(
        git_dir = %repo.git_dir().display(),
        ref_name = %name,
        "keeping worktree: ref exists only in this repo"
    );
    Some(KeepReason::WorktreeLocalState(name))
}

pub(super) fn find_registered_child(repo: &gix::Repository) -> Option<KeepReason> {
    match has_entry_outside(&repo.git_dir().join("worktrees"), &[]) {
        Ok(false) => None,
        Ok(true) => {
            tracing::info!(
                git_dir = %repo.git_dir().display(),
                "keeping worktree: child worktree registered only here"
            );
            Some(KeepReason::WorktreeLocalState("worktrees".to_string()))
        }
        Err(error) => {
            tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to read worktrees registrations");
            Some(KeepReason::CheckFailed)
        }
    }
}

pub(super) fn find_child_registration_state(repo: &gix::Repository) -> Option<KeepReason> {
    let registrations = repo.git_dir().join("worktrees");
    let entries = match read_dir_if_present(&registrations) {
        Ok(None) => return None,
        Ok(Some(entries)) => entries,
        Err(error) => {
            tracing::warn!(path = %registrations.display(), %error, "failed to read worktrees directory");
            return Some(KeepReason::CheckFailed);
        }
    };
    for entry in entries {
        let name = match entry {
            Ok(entry) => entry.file_name(),
            Err(error) => {
                tracing::warn!(path = %registrations.display(), %error, "failed to read a worktree registration entry");
                return Some(KeepReason::CheckFailed);
            }
        };
        let name = name.to_string_lossy().into_owned();
        let registration = registrations.join(&name);
        match read_state_entry(&registration) {
            Ok(None) => {}
            Ok(Some(held)) => {
                tracing::info!(
                    git_dir = %repo.git_dir().display(),
                    registration = %name,
                    entry = %held,
                    "keeping worktree: child registration holds worktree-local state"
                );
                return Some(KeepReason::WorktreeLocalState(format!(
                    "worktrees/{name}/{held}"
                )));
            }
            Err(error) => {
                tracing::warn!(path = %registrations.display(), %error, "failed to read a worktree registration's state");
                return Some(KeepReason::CheckFailed);
            }
        }
    }
    None
}

pub(super) fn find_operation_in_progress(git_dir: &Path) -> Option<KeepReason> {
    match read_operation_in_progress(git_dir) {
        Ok(None) => None,
        Ok(Some(name)) => {
            tracing::info!(git_dir = %git_dir.display(), entry = %name, "keeping worktree: operation in progress");
            Some(KeepReason::WorktreeLocalState(name))
        }
        Err(error) => keep_on_unreadable(git_dir, &error),
    }
}

fn read_operation_in_progress(git_dir: &Path) -> std::io::Result<Option<String>> {
    let Some(entries) = read_dir_if_present(git_dir)? else {
        return Ok(None);
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if OPERATION_IN_PROGRESS.contains(&name.as_str()) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

fn read_state_entry(git_dir: &Path) -> std::io::Result<Option<String>> {
    let Some(entries) = read_dir_if_present(git_dir)? else {
        return Ok(None);
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let holds_state = match name.as_str() {
            "refs" => has_any_file(&entry.path())?,
            "info" => has_entry_outside(&entry.path(), REMOVABLE_INFO_ENTRIES)?,
            "config.worktree" => !is_checkout_shape_only(&entry.path()),
            name => {
                !REMOVABLE_REGISTRATION_ENTRIES.contains(&name)
                    && !name.starts_with(REMOVABLE_REGISTRATION_PREFIX)
            }
        };
        if holds_state {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

fn is_checkout_shape_only(config: &Path) -> bool {
    let Ok(file) =
        gix::config::File::from_path_no_includes(config.to_owned(), gix::config::Source::Worktree)
    else {
        tracing::warn!(path = %config.display(), "failed to read worktree config");
        return false;
    };
    file.sections().all(|section| {
        let header = section.header();
        header.subsection_name().is_none()
            && section.value_names().all(|key| {
                let full = format!("{}.{key}", header.name()).to_lowercase();
                CHECKOUT_SHAPE_KEYS.contains(&full.as_str())
            })
    })
}
