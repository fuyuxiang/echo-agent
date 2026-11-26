//! The delete gate: decides whether removing a worktree would lose anything.
//! Every failure, timeout, or unreadable path is a keep — the gate fails toward
//! keep. One module per question; see
//! `docs/internal/automatic-worktree-cleanup.md`.

pub use super::reason::KeepReason;
pub(crate) use super::reason::Safety;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::checkout::git_command;

mod build_output;
mod conversions;
mod git_dir;
mod reachability;
mod refs;
mod working_tree;

pub(crate) use reachability::collect_reflog_tips;

use conversions::find_converted_path;
use git_dir::{
    find_child_registration_state, find_dying_stores, find_operation_in_progress,
    find_own_registration_state, find_repo_local_state,
};
#[cfg(test)]
use reachability::find_reflog_only_commits;
use reachability::{find_missing_objects, find_unpushed_commits};
use working_tree::{find_gitlink_content, find_working_tree_content};

const NO_SUCH_PATH: &str = "/nonexistent/echo-agent-fast-worktree/no-such-file";

/// Whether `<worktree>/.git` is *definitively* absent. `NotFound` and
/// `NotADirectory` (a non-dir sits where the worktree should be, so nothing can
/// live inside it) are definitive; any other stat error is "couldn't tell" and
/// must not be read as absence (the caller keeps rather than treating as NoRepo).
pub(super) fn git_entry_definitely_absent(worktree: &Path) -> bool {
    match worktree.join(".git").try_exists() {
        Ok(there) => !there,
        Err(error) => error.kind() == std::io::ErrorKind::NotADirectory,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Captured<'a> {
    Nothing,
    WorkingTreeSnapshot(&'a str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ownership {
    Linked,
    Standalone,
}

// Production always names reflog-only commits itself (reclaimed.rs) before
// deleting, so the gate leaves that question to the caller. These test-only
// entry points layer the standalone reflog-only keep back on top of the verdict
// so the gate's own conservative behavior can be exercised directly.
#[cfg(test)]
pub(crate) fn safe_to_delete_worktree(worktree: &Path, surviving: Option<&Path>) -> Safety {
    keep_on_reflog_only(
        worktree,
        safe_to_delete_named_worktree(worktree, surviving, None, None),
    )
}

#[cfg(test)]
fn safe_to_delete_worktree_after_snapshot(
    worktree: &Path,
    surviving: Option<&Path>,
    snapshot: &str,
) -> Safety {
    keep_on_reflog_only(
        worktree,
        safe_to_delete_named_worktree(worktree, surviving, Some(snapshot), None),
    )
}

/// Turn a `Delete` verdict into a keep when the worktree names commits only its
/// reflog holds — the conservative behavior production replaces with naming.
#[cfg(test)]
fn keep_on_reflog_only(worktree: &Path, verdict: Safety) -> Safety {
    if verdict != Safety::Delete {
        return verdict;
    }
    match gix::open(worktree)
        .ok()
        .and_then(|repo| find_reflog_only_commits(worktree, &repo))
    {
        Some(reason) => Safety::Keep(reason),
        None => Safety::Delete,
    }
}

pub(super) fn safe_to_delete_named_worktree(
    worktree: &Path,
    surviving: Option<&Path>,
    snapshot: Option<&str>,
    timeout: Option<Duration>,
) -> Safety {
    decide_within(
        worktree,
        surviving,
        snapshot,
        timeout.unwrap_or(GATE_TIMEOUT),
    )
}

fn decide_within(
    worktree: &Path,
    surviving: Option<&Path>,
    snapshot: Option<&str>,
    timeout: Duration,
) -> Safety {
    let (path, surviving, snapshot) = (
        worktree.to_owned(),
        surviving.map(Path::to_owned),
        snapshot.map(|snapshot| snapshot.to_owned()),
    );
    answer_within(worktree, timeout, move || {
        let captured = match &snapshot {
            Some(snapshot) => Captured::WorkingTreeSnapshot(snapshot),
            None => Captured::Nothing,
        };
        decide_safety(&path, surviving.as_deref(), captured)
    })
}

const GATE_TIMEOUT: Duration = Duration::from_secs(600);

fn answer_within(
    worktree: &Path,
    timeout: Duration,
    gate: impl FnOnce() -> Safety + Send + 'static,
) -> Safety {
    let (sent, answered) = std::sync::mpsc::channel();
    // The gate runs on its own thread so a git call that hangs in-process (no
    // timeout reaches it) does not block the pass: on timeout we abandon the
    // thread and keep the worktree. If the thread cannot even start, keep too.
    if std::thread::Builder::new()
        .name("worktree-safety-gate".into())
        .spawn(move || {
            let _ = sent.send(gate());
        })
        .is_err()
    {
        tracing::warn!(path = %worktree.display(), "safety gate could not start; keeping worktree");
        return Safety::Keep(KeepReason::CheckFailed);
    }
    match answered.recv_timeout(timeout) {
        Ok(safety) => safety,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            tracing::warn!(
                path = %worktree.display(),
                ?timeout,
                "safety gate timed out; keeping worktree"
            );
            Safety::Keep(KeepReason::GateTimedOut)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            tracing::warn!(path = %worktree.display(), "safety gate panicked; keeping worktree");
            Safety::Keep(KeepReason::CheckFailed)
        }
    }
}

fn decide_safety(worktree: &Path, surviving: Option<&Path>, captured: Captured<'_>) -> Safety {
    let repo = match gix::open(worktree) {
        Ok(repo) => repo,
        Err(error) => {
            // Only a definitively-absent `.git` is NoRepo (a removable class
            // downstream); an unreadable one is "couldn't tell" → CheckFailed.
            let reason = if git_entry_definitely_absent(worktree) {
                KeepReason::NoRepo
            } else {
                KeepReason::CheckFailed
            };
            tracing::warn!(
                path = %worktree.display(),
                %error,
                ?reason,
                "path did not open as a git repository"
            );
            return Safety::Keep(reason);
        }
    };
    let ownership = if repo.common_dir() == repo.git_dir() {
        Ownership::Standalone
    } else {
        Ownership::Linked
    };
    if let Some(reason) =
        find_working_tree_content(&repo, captured).or_else(|| find_gitlink_content(&repo))
    {
        return Safety::Keep(reason);
    }
    let surviving = match ownership {
        Ownership::Standalone => open_surviving_repo(worktree, &repo, surviving),
        Ownership::Linked => None,
    };
    let reason = match ownership {
        Ownership::Standalone => decide_standalone(worktree, &repo, surviving.as_ref()),
        Ownership::Linked => decide_linked(worktree, &repo, captured),
    };
    if let Some(reason) = reason {
        return Safety::Keep(reason);
    }
    match captured {
        Captured::Nothing => Safety::Delete,
        Captured::WorkingTreeSnapshot(snapshot) => {
            if let Some(reason) = find_converted_path(worktree, snapshot) {
                return Safety::Keep(reason);
            }
            match super::checkout::worktree_matches_snapshot(worktree, snapshot) {
                Ok(true) => Safety::Delete,
                Ok(false) => {
                    tracing::info!(
                        path = %worktree.display(),
                        %snapshot,
                        "working tree changed since the snapshot"
                    );
                    Safety::Keep(KeepReason::Dirty)
                }
                Err(error) => {
                    tracing::warn!(path = %worktree.display(), %error, "failed to compare working tree against snapshot");
                    Safety::Keep(KeepReason::CheckFailed)
                }
            }
        }
    }
}

fn decide_standalone(
    worktree: &Path,
    repo: &gix::Repository,
    surviving: Option<&gix::Repository>,
) -> Option<KeepReason> {
    match surviving {
        Some(surviving) => find_missing_objects(repo, surviving)
            .or_else(|| find_dying_stores(repo, Some(surviving)))
            .or_else(|| find_child_registration_state(repo))
            .or_else(|| find_operation_in_progress(repo.git_dir())),
        // No survivor to prove reachability against, so a snapshot ref proves
        // nothing here: treat capture as `Nothing` and lean on unpushed/local checks.
        None => find_unpushed_commits(worktree, Ownership::Standalone, Captured::Nothing)
            .or_else(|| find_dying_stores(repo, None))
            .or_else(|| find_repo_local_state(repo)),
    }
}

fn decide_linked(
    worktree: &Path,
    repo: &gix::Repository,
    captured: Captured<'_>,
) -> Option<KeepReason> {
    find_unpushed_commits(worktree, Ownership::Linked, captured)
        .or_else(|| find_own_registration_state(repo.git_dir()))
}

fn outlives(worktree: &Path, candidate: &Path) -> bool {
    // Couldn't tell (either side fails to canonicalize) ⇒ do not trust the
    // candidate as a surviving store; caller then judges the worktree alone.
    let (Ok(worktree), Ok(candidate)) = (
        dunce::canonicalize(worktree),
        dunce::canonicalize(candidate),
    ) else {
        return false;
    };
    !candidate.starts_with(&worktree)
}

fn open_surviving_repo(
    worktree: &Path,
    worktree_repo: &gix::Repository,
    surviving: Option<&Path>,
) -> Option<gix::Repository> {
    let surviving = surviving?;
    if !outlives(worktree, surviving) {
        tracing::warn!(
            path = %worktree.display(),
            surviving = %surviving.display(),
            "surviving repo is inside the worktree; ignoring it"
        );
        return None;
    }
    match gix::open(surviving) {
        Ok(opened) if opened.common_dir() == worktree_repo.common_dir() => {
            tracing::warn!(
                path = %worktree.display(),
                surviving = %surviving.display(),
                "surviving repo is the worktree's own; ignoring it"
            );
            None
        }
        Ok(opened) => Some(opened),
        Err(error) => {
            tracing::warn!(
                path = %worktree.display(),
                surviving = %surviving.display(),
                %error,
                "surviving repository failed to open; judging the worktree alone"
            );
            None
        }
    }
}

pub(super) fn probe_command(worktree: &Path) -> Command {
    let mut command = git_command();
    command.current_dir(worktree);
    command.args([
        "--literal-pathspecs",
        "--no-replace-objects",
        "-c",
        "core.fsmonitor=false",
        "-c",
        &format!("core.hooksPath={}", super::checkout::NO_HOOKS),
    ]);
    command.env("GIT_GRAFT_FILE", NO_SUCH_PATH);
    super::probe::forget_inherited_git_environment(&mut command);
    command
}

#[cfg(test)]
#[path = "safety_tests.rs"]
mod tests;
